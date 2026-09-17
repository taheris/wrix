# wrix-builder: Darwin CLI wrapper for a Linux remote builder
#
# Manages a container that serves as an ssh-ng:// remote builder
# for Nix on macOS. Uses Apple's container CLI (macOS 26+).
#
# Usage:
#   wrix-builder start   - Start the builder container.
#   wrix-builder stop    - Stop and remove the container.
#   wrix-builder status  - Show builder status.
#   wrix-builder ssh     - Connect to builder via SSH.
#   wrix-builder config  - Print nix.conf snippet for remote builder.
#
{
  pkgs,
  linuxPkgs,
  asTarball ? true,
  builderImage ? import ../sandbox/builder/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    inherit asTarball;
  },
}:

let
  shellLib = import ../util/shell.nix { inherit pkgs; };
  builderSystem = linuxPkgs.stdenv.hostPlatform.system;
  builderSeedRoots = builderImage.darwin_seed_roots or [ ];
  builderStoreExport = import ./darwin-store-export.nix {
    inherit pkgs;
    seedRoots = builderSeedRoots;
  };

  script = pkgs.writeShellScriptBin "wrix-builder" ''
      set -euo pipefail
      . ${./keys.sh}
      . ${./darwin-store.sh}
      WRIX_BUILDER_SSH_KEYGEN="''${WRIX_BUILDER_SSH_KEYGEN:-${pkgs.openssh}/bin/ssh-keygen}"
      WRIX_BUILDER_BASE64="''${WRIX_BUILDER_BASE64:-${pkgs.coreutils}/bin/base64}"
      WRIX_BUILDER_SKOPEO="''${WRIX_BUILDER_SKOPEO:-${pkgs.skopeo}/bin/skopeo}"
      WRIX_BUILDER_JQ="''${WRIX_BUILDER_JQ:-${pkgs.jq}/bin/jq}"
      WRIX_BUILDER_STORE_EXPORT="''${WRIX_BUILDER_STORE_EXPORT:-${builderStoreExport}}"

      resolve_user_home() {
        local user="$1"
        local home=""
        if command -v dscl >/dev/null 2>&1; then
          home=$(dscl . -read "/Users/$user" NFSHomeDirectory | awk '{ print $2 }') || home=""
        fi
        if [[ -z "$home" ]] && command -v getent >/dev/null 2>&1; then
          home=$(getent passwd "$user" | cut -d: -f6) || home=""
        fi
        [[ -n "$home" ]] || home="$HOME"
        printf '%s\n' "$home"
      }

      BUILDER_HOST_HOME="$HOME"
      if [[ "$(id -u)" -eq 0 && -n "''${SUDO_USER:-}" ]]; then
        BUILDER_HOST_HOME=$(resolve_user_home "$SUDO_USER")
      fi

      XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$BUILDER_HOST_HOME/.cache}"
      WRIX_DATA="$BUILDER_HOST_HOME/.local/share/wrix"
      WRIX_CACHE="$XDG_CACHE_HOME/wrix"

      BUILDER_KEYS_DIR="$WRIX_DATA/builder-keys"
      HOST_KEY="$BUILDER_KEYS_DIR/host_ed25519"
      HOST_KEY_BASE64="$BUILDER_KEYS_DIR/public_host_key_base64"
      CLIENT_KEY="$BUILDER_KEYS_DIR/client_ed25519"
      CLIENT_KNOWN_HOSTS="$BUILDER_KEYS_DIR/known_hosts"
      SYSTEM_CLIENT_KEY="/etc/nix/wrix_builder_ed25519"
      SYSTEM_HOST_KEY="/etc/nix/wrix_builder_host_key_base64"
      LEGACY_NIX_STORE="$WRIX_DATA/builder-nix"
      NIX_VOLUME="wrix-builder-nix"
      NIX_VOLUME_SIZE="40G"
      NIX_VOLUME_VERSION_FILE="$WRIX_DATA/builder-volume-image-version"
      CONTAINER_NAME="wrix-builder"
      BUILDER_IMAGE="${builderImage.ref}"
      BUILDER_IMAGE_SOURCE="${builderImage.source}"
      BUILDER_IMAGE_SOURCE_KIND="${builderImage.source_kind}"
      BUILDER_IMAGE_DIGEST="${builderImage.digest}"
      BUILDER_SYSTEM="${builderSystem}"
      SSH_PORT=2222

      usage() {
        echo "Usage: wrix-builder <command>"
        echo ""
        echo "Commands:"
        echo "  start        - Start the builder container."
        echo "  stop         - Stop and remove the container."
        echo "  status       - Show builder status and SSH connection info."
        echo "  ssh          - Connect to builder via SSH or run a command."
        echo "  config       - Print nix-darwin configuration for remote builder."
        echo "  setup        - Configure routes and SSH using sudo."
        echo "  setup-routes - Configure container network routes using sudo."
        echo "  setup-ssh    - Add the host key to root's known_hosts using sudo."
        exit 1
      }

      container_name_exists() {
        local name="$1"
        local output
        output=$(container inspect "$name" 2>/dev/null) || return 1
        [[ "$output" != "[]" ]]
      }

      container_exists() {
        container_name_exists "$CONTAINER_NAME"
      }

      container_state_from_json() {
        "$WRIX_BUILDER_JQ" -er '
          ((if type == "array" then .[0] else . end) // {}) as $container
          | if ($container.status | type) == "object" then
              $container.status.state
            else
              $container.state // $container.status
            end
          | strings
          | select(length > 0)
        '
      }

      container_state() {
        local name="$1"
        container inspect "$name" 2>/dev/null | container_state_from_json
      }

      cleanup_container() {
        local name="$1"
        if ! container_name_exists "$name"; then
          return 0
        fi
        if ! container stop "$name" >/dev/null 2>&1; then
          echo "Warning: could not stop container $name during cleanup" >&2
        fi
        if ! container rm "$name" >/dev/null 2>&1; then
          echo "Warning: could not remove container $name during cleanup" >&2
        fi
      }

      ensure_ssh_client_material() {
        wrix_builder_ensure_key_material "$BUILDER_KEYS_DIR" "$SSH_PORT"
      }

      require_ssh_client_material() {
        wrix_builder_require_key_material "$BUILDER_KEYS_DIR"
      }

      check_macos_version() {
        if [[ "$(sw_vers -productVersion | cut -d. -f1)" -lt 26 ]]; then
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

      builder_ssh_ready() {
        ssh \
          -p "$SSH_PORT" \
          -i "$CLIENT_KEY" \
          -o BatchMode=yes \
          -o ConnectTimeout=1 \
          -o IdentitiesOnly=yes \
          -o StrictHostKeyChecking=yes \
          -o UserKnownHostsFile="$CLIENT_KNOWN_HOSTS" \
          builder@localhost true >/dev/null 2>&1
      }

      wait_for_builder_services() {
        local i

        for i in {1..120}; do
          if container exec "$CONTAINER_NAME" pgrep -x nix-daemon >/dev/null 2>&1 && builder_ssh_ready; then
            return 0
          fi
          if [[ "$i" -lt 120 ]]; then
            sleep 1
          fi
        done

        echo "Error: nix-daemon and SSH did not become ready within 120 seconds" >&2
        cleanup_container "$CONTAINER_NAME"
        return 1
      }

      builder_image_digest() {
        local digest_source="$BUILDER_IMAGE_DIGEST"
        local digest=""

        if [[ "$digest_source" =~ ^sha256:[0-9a-f]{64}$ ]]; then
          digest="$digest_source"
        elif [[ -s "$digest_source" ]]; then
          digest=$(cat "$digest_source")
        else
          echo "Error: builder image digest is missing: $digest_source" >&2
          exit 1
        fi

        if [[ ! "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
          echo "Error: builder image digest is invalid: $digest" >&2
          exit 1
        fi
        printf '%s\n' "$digest"
      }

      builder_image_skopeo_source() {
        local oci_layout
        local oci_ref

        case "$BUILDER_IMAGE_SOURCE_KIND" in
          docker-archive)
            printf 'docker-archive:%s\n' "$BUILDER_IMAGE_SOURCE"
            ;;
          nix-descriptor)
            oci_layout=$("$WRIX_BUILDER_JQ" -er '.oci_layout // empty | strings | select(length > 0)' "$BUILDER_IMAGE_SOURCE") || {
              echo "Error: nix-descriptor builder image source is missing oci_layout: $BUILDER_IMAGE_SOURCE" >&2
              exit 1
            }
            oci_ref=$("$WRIX_BUILDER_JQ" -er '.oci_ref // "latest" | strings | select(length > 0)' "$BUILDER_IMAGE_SOURCE") || {
              echo "Error: nix-descriptor builder image source has an invalid oci_ref: $BUILDER_IMAGE_SOURCE" >&2
              exit 1
            }
            printf 'oci:%s:%s\n' "$oci_layout" "$oci_ref"
            ;;
          *)
            echo "Error: unsupported builder image source_kind: $BUILDER_IMAGE_SOURCE_KIND" >&2
            exit 1
            ;;
        esac
      }

      container_image_refs() {
        local list_output

        if ! list_output=$(container image list --format json); then
          echo "Warning: could not list images for builder cleanup" >&2
          return 0
        fi

        printf '%s\n' "$list_output" | "$WRIX_BUILDER_JQ" -r '
          (if type == "array" then .[] else . end)
          | .configuration.name // empty
          | strings
          | select(length > 0)
          | sub("^docker.io/library/"; "")
        '
      }

      builder_image_json_digest() {
        "$WRIX_BUILDER_JQ" -r '
          ((if type == "array" then .[0] else . end) // {})
          | .digest // .id // .Id // empty
        '
      }

      builder_image_json_has_builder_labels() {
        "$WRIX_BUILDER_JQ" -e '
          ((if type == "array" then .[0] else . end) // {}) as $image
          | ([
              $image.labels?,
              $image.Labels?,
              $image.config.labels?,
              $image.config.Labels?,
              $image.Config.Labels?,
              $image.variants[]?.config.config.Labels?
            ] | map(select(type == "object")) | add // {}) as $labels
          | $labels["wrix.managed"] == "true" and $labels["wrix.image.kind"] == "builder"
        ' >/dev/null
      }

      builder_image_is_legacy_ref() {
        local ref="$1"

        [[ "$ref" == wrix-builder:* ]]
      }

      cleanup_stale_builder_images() {
        local current_image="$1"
        local current_short="''${current_image#sha256:}"
        local image_json
        local ref
        local ref_digest
        local ref_short

        while IFS= read -r ref; do
          [[ -n "$ref" ]] || continue
          [[ "$ref" != "$BUILDER_IMAGE" ]] || continue
          [[ "$ref" != "wrix-builder:latest" ]] || continue

          if ! image_json=$(container image inspect "$ref"); then
            echo "Warning: could not inspect image $ref during builder cleanup" >&2
            continue
          fi

          ref_digest=$(printf '%s\n' "$image_json" | builder_image_json_digest)
          if [[ -z "$ref_digest" && "$ref" == untagged@sha256:* ]]; then
            ref_digest="sha256:''${ref#untagged@sha256:}"
          fi
          ref_short="''${ref_digest#sha256:}"
          if [[ -n "$ref_short" && "$ref_short" == "$current_short" ]]; then
            continue
          fi

          if printf '%s\n' "$image_json" | builder_image_json_has_builder_labels || builder_image_is_legacy_ref "$ref"; then
            if ! container image delete "$ref" >/dev/null; then
              echo "Warning: could not delete stale builder image $ref" >&2
            fi
          fi
        done < <(container_image_refs)
      }

      load_builder_image() {
        local current_image
        local loaded_ref
        local load_output
        local needs_load=false
        local oci_tar
        local source_ref
        local store_version_file="$NIX_VOLUME_VERSION_FILE"

        current_image=$(builder_image_digest)
        if ! container image inspect "$BUILDER_IMAGE" >/dev/null 2>&1; then
          needs_load=true
        elif [[ ! -f "$store_version_file" || "$(cat "$store_version_file")" != "$current_image" ]]; then
          needs_load=true
        fi

        if [[ "$needs_load" == true ]]; then
          echo "Loading builder image..."
          if container image inspect "$BUILDER_IMAGE" >/dev/null 2>&1; then
            container image delete "$BUILDER_IMAGE"
          fi
          if [[ "$BUILDER_IMAGE" != "wrix-builder:latest" ]]; then
            container image delete "wrix-builder:latest" 2>/dev/null || true # best-effort: the legacy tag may be absent before the new image is loaded.
          fi
          source_ref=$(builder_image_skopeo_source)
          oci_tar="$WRIX_CACHE/builder-image-oci.tar"
          mkdir -p "$WRIX_CACHE"
          "$WRIX_BUILDER_SKOPEO" --insecure-policy copy --quiet "$source_ref" "oci-archive:$oci_tar"
          load_output=$(container image load --input "$oci_tar" 2>&1)
          loaded_ref=$(printf '%s\n' "$load_output" | awk 'match($0, /untagged@sha256:[a-f0-9]+/) { print substr($0, RSTART, RLENGTH); exit }')
          if [[ -z "$loaded_ref" ]]; then
            echo "Error: Could not determine loaded builder image reference" >&2
            printf '%s\n' "$load_output" >&2
            exit 1
          fi
          container image tag "$loaded_ref" "$BUILDER_IMAGE"
          container image tag "$loaded_ref" "wrix-builder:latest" 2>/dev/null || true # best-effort: the hash ref is authoritative when the legacy alias cannot be updated.
          if ! container image delete "$loaded_ref" >/dev/null; then
            echo "Error: Could not delete temporary loaded builder image $loaded_ref" >&2
            exit 1
          fi
          rm -f "$oci_tar"
          cleanup_stale_builder_images "$current_image"
        fi
      }

      cmd_start() {
        local current_image
        local needs_init=false
        local seed_container
        local state

        current_image=$(builder_image_digest)
        check_macos_version
        ensure_container_system

        if container_exists; then
          state=$(container_state "$CONTAINER_NAME")
          if [[ "$state" == "running" ]]; then
            echo "Builder container is already running"
            echo "Use 'wrix-builder ssh' to connect"
            exit 0
          fi
          echo "Cleaning up stale container..."
          container rm "$CONTAINER_NAME"
        fi

        ensure_ssh_client_material
        load_builder_image

        if ! wrix_builder_nix_volume_exists; then
          needs_init=true
          echo "Initializing persistent Nix store (first run)..."
        elif ! wrix_builder_nix_volume_is_managed; then
          echo "Error: refusing to use unmanaged volume $NIX_VOLUME" >&2
          exit 1
        elif [[ ! -f "$NIX_VOLUME_VERSION_FILE" || "$(cat "$NIX_VOLUME_VERSION_FILE")" != "$current_image" ]]; then
          needs_init=true
          echo "Builder image changed, re-initializing persistent Nix store..."
        fi

        if [[ "$needs_init" == true ]]; then
          echo "This may take a few minutes..."
          wrix_builder_remove_nix_volume
          wrix_builder_create_nix_volume

          seed_container="wrix-builder-seed-$$"
          if ! wrix_builder_seed_nix_volume "$seed_container"; then
            exit 1
          fi

          mkdir -p "$WRIX_DATA"
          echo "$current_image" > "$NIX_VOLUME_VERSION_FILE"
          echo "Nix store initialized and verified"
        fi

        if [[ -d "$LEGACY_NIX_STORE" ]]; then
          echo "Legacy Nix store preserved at $LEGACY_NIX_STORE (not used)"
        fi

        echo "Creating builder container..."

        container run \
          --name "$CONTAINER_NAME" \
          -d \
          -c 4 \
          -m 4096M \
          --network default \
          -p "127.0.0.1:$SSH_PORT:22" \
          -v "$BUILDER_KEYS_DIR:/run/keys:ro" \
          -v "$NIX_VOLUME:/nix" \
          "$BUILDER_IMAGE"

        ${shellLib.fixVmnetRoute}

        echo "Waiting for services to start..."
        wait_for_builder_services

        echo ""
        echo "Builder started successfully!"
        echo ""
        echo "Next steps:"
        echo "  wrix-builder setup   - Configure routes and SSH for nix-daemon using sudo."
        echo "  wrix-builder ssh     - Connect to builder via SSH."
        echo "  wrix-builder config  - Print nix-darwin configuration."
      }

      cmd_stop() {
        echo "Stopping builder container..."
        cleanup_container "$CONTAINER_NAME"
        echo "Builder stopped"
      }

      cmd_status() {
        local state

        if container_exists; then
          state=$(container_state "$CONTAINER_NAME")
          echo "Builder: $state"
          echo ""
          echo "SSH connection:"
          echo "  wrix-builder ssh"
          echo ""
          echo "SSH keys: $BUILDER_KEYS_DIR"
          echo "Nix store volume: $NIX_VOLUME ($NIX_VOLUME_SIZE)"
        else
          echo "Builder: not created"
          echo ""
          echo "Nix store volume: $NIX_VOLUME ($NIX_VOLUME_SIZE)"
          echo "Run 'wrix-builder start' to create and start the builder"
        fi
        if [[ -d "$LEGACY_NIX_STORE" ]]; then
          echo "Legacy Nix store: $LEGACY_NIX_STORE (preserved; not used)"
        fi
      }

      cmd_ssh() {
        local -a ssh_args
        local state

        if ! container_exists; then
          echo "Error: Builder is not running"
          echo "Run 'wrix-builder start' first"
          exit 1
        fi

        state=$(container_state "$CONTAINER_NAME")
        if [[ "$state" != "running" ]]; then
          echo "Error: Builder container is not running (state: $state)"
          echo "Run 'wrix-builder start' first"
          exit 1
        fi

        ensure_ssh_client_material
        ssh_args=(
          -p "$SSH_PORT"
          -i "$CLIENT_KEY"
          -o BatchMode=yes
          -o IdentitiesOnly=yes
          -o StrictHostKeyChecking=yes
          -o UserKnownHostsFile="$CLIENT_KNOWN_HOSTS"
        )

        if [[ $# -eq 0 ]]; then
          exec ssh "''${ssh_args[@]}" "builder@localhost"
        else
          exec ssh "''${ssh_args[@]}" "builder@localhost" "$@"
        fi
      }

      cmd_config() {
        cat <<NIXCONFIG
    # Add to nix-darwin configuration:

    # Run wrix-builder setup before using this module's builder.

    {
      nix.buildMachines = [
        {
          hostName = "localhost:2222";
          systems = [ "$BUILDER_SYSTEM" ];
          protocol = "ssh-ng";
          sshUser = "builder";
          sshKey = "$SYSTEM_CLIENT_KEY";
          maxJobs = 4;
          speedFactor = 1;
          supportedFeatures = [ "big-parallel" "benchmark" ];
        }
      ];
    }
    NIXCONFIG
      }

      cmd_setup_routes() {
        ${shellLib.fixVmnetRoute}
      }

      cmd_setup_ssh() {
        local -a container_cmd=(container)
        local host_key
        local output
        local root_known_hosts="/var/root/.ssh/known_hosts"
        local state

        if [[ "$(id -u)" -ne 0 ]]; then
          sudo "$0" setup-ssh
          return
        fi

        require_ssh_client_material

        if [[ -n "''${SUDO_USER:-}" ]]; then
          container_cmd=(sudo -u "$SUDO_USER" container)
        fi

        if ! output=$("''${container_cmd[@]}" inspect "$CONTAINER_NAME" 2>/dev/null); then
          output=""
        fi
        if [[ -z "$output" || "$output" == "[]" ]]; then
          echo "Error: Builder container is not running"
          echo "Run 'wrix-builder start' first"
          exit 1
        fi

        state=$(printf '%s\n' "$output" | container_state_from_json)
        if [[ "$state" != "running" ]]; then
          echo "Error: Builder container is not running (state: $state)"
          echo "Run 'wrix-builder start' first"
          exit 1
        fi

        mkdir -p /etc/nix

        echo "Installing SSH key at $SYSTEM_CLIENT_KEY..."
        rm -f "$SYSTEM_CLIENT_KEY"
        cp "$CLIENT_KEY" "$SYSTEM_CLIENT_KEY"
        chmod 600 "$SYSTEM_CLIENT_KEY"
        chown root:nixbld "$SYSTEM_CLIENT_KEY"

        echo "Installing host key metadata at $SYSTEM_HOST_KEY..."
        rm -f "$SYSTEM_HOST_KEY"
        cp "$HOST_KEY_BASE64" "$SYSTEM_HOST_KEY"
        chmod 644 "$SYSTEM_HOST_KEY"

        echo "Adding to root's known_hosts..."
        host_key=$(cat "$HOST_KEY.pub")
        mkdir -p /var/root/.ssh
        chmod 700 /var/root/.ssh

        if [[ -f "$root_known_hosts" ]]; then
          awk -v port="$SSH_PORT" '$1 != "wrix-builder" && $1 != "[localhost]:" port { print }' "$root_known_hosts" > "$root_known_hosts.tmp"
          mv "$root_known_hosts.tmp" "$root_known_hosts"
        fi

        {
          printf 'wrix-builder %s\n' "$host_key"
          printf '[localhost]:%s %s\n' "$SSH_PORT" "$host_key"
        } >> "$root_known_hosts"
        chmod 600 "$root_known_hosts"

        if ! ssh -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$root_known_hosts" -i "$SYSTEM_CLIENT_KEY" -p "$SSH_PORT" builder@localhost true 2>/dev/null; then
          echo "Error: Failed to connect to wrix-builder"
          echo "Check that routes are configured: wrix-builder setup-routes"
          exit 1
        fi
        echo "Host key added to known_hosts"
      }

      cmd_setup() {
        echo "=== Setting up wrix-builder ==="
        echo ""
        echo "Step 1: Configuring network routes..."
        cmd_setup_routes
        echo ""
        echo "Step 2: Adding SSH host key for root..."
        cmd_setup_ssh
        echo ""
        echo "=== Setup complete ==="
        echo "You can now use 'nix run' with wrix-builder as a remote builder"
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
    publicHostKeyFile = "/etc/nix/wrix_builder_host_key_base64";
    sshKey = "/etc/nix/wrix_builder_ed25519";
    keysPath = "~/.local/share/wrix/builder-keys";
  };
})
