# Darwin sandbox using Apple container CLI (macOS 26+)
{ pkgs }:

let
  inherit (builtins) readFile;
  inherit (paths) mkMountSpecs;
  inherit (pkgs) writeShellScriptBin writeTextDir;
  inherit (shellLib)
    expandPathFn
    cleanStaleStagingDirs
    createStagingDir
    mkDeployKeyExpr
    stageBeads
    ;

  imageTagLib = import ../../util/image-tag.nix { };
  knownHosts = import ../known-hosts.nix { inherit pkgs; };
  paths = import ../../util/path.nix { };
  shellLib = import ../../util/shell.nix { };
  sshConfig = import ../../util/ssh.nix;

  promptDir = writeTextDir "wrapix-prompt" (readFile ../prompt.txt);

in
{
  mkSandbox =
    {
      profile,
      profileImage,
      cpus ? null,
      memoryMb ? 4096,
      deployKey ? null,
      networkAllowlist ? "",
      ...
    }:
    let
      deployKeyExpr = mkDeployKeyExpr deployKey;

    in
    writeShellScriptBin "wrapix" ''
            set -euo pipefail

            # Verbose mode for debugging startup
            WRAPIX_VERBOSE="''${WRAPIX_VERBOSE:-}"
            verbose() { [ -n "$WRAPIX_VERBOSE" ] && echo "[wrapix] $*" >&2 || true; }

            # Ensure USER is set (may be unset in some environments)
            USER="''${USER:-$(id -un)}"

            # XDG-compliant directories for staging and image cache
            XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}"
            WRAPIX_CACHE="$XDG_CACHE_HOME/wrapix"
            PROJECT_DIR="''${1:-$(pwd)}"
            shift || true
            # Remaining args override the container command (passed to entrypoint as $@)
            CONTAINER_CMD=()
            if [ $# -gt 0 ]; then
              CONTAINER_CMD=("$@")
            fi

            # Check macOS version
            if [ "$(sw_vers -productVersion | cut -d. -f1)" -lt 26 ]; then
              echo "Error: macOS 26+ required (current: $(sw_vers -productVersion))"
              exit 1
            fi

            # Ensure the per-workspace wrapix-beads dolt container is running.
            # Requires host podman (typically via 'podman machine'); skipped
            # if podman isn't available, same as the devShell shellHook.
            if [ -d "$PROJECT_DIR/.beads/dolt" ] && command -v podman >/dev/null 2>&1; then
              ${pkgs.beads-dolt}/bin/beads-dolt start "$PROJECT_DIR"
            fi

            # Ensure container system is running
            if ! container system status >/dev/null 2>&1; then
              echo "Starting container system..." >&2
              container system start
              sleep 2
            fi

            # Load profile image using hash-based tag — no version file needed.
            PROFILE_IMAGE="wrapix-${profile.name}:${imageTagLib.mkImageTag profileImage}"
            if ! container image inspect "$PROFILE_IMAGE" >/dev/null 2>&1; then
              verbose "Image hash changed or missing, reloading..."
              echo "Loading profile image..."
              container image delete "wrapix-${profile.name}:latest" 2>/dev/null || true
              # Convert Docker-format tar to OCI-archive for Apple container CLI.
              # --insecure-policy is safe: images are built locally from Nix
              # derivations (trusted source with cryptographic hashes).
              OCI_TAR="$WRAPIX_CACHE/profile-image-oci.tar"
              mkdir -p "$WRAPIX_CACHE"
              ${pkgs.skopeo}/bin/skopeo --insecure-policy copy --quiet "docker-archive:${profileImage}" "oci-archive:$OCI_TAR"
              LOAD_OUTPUT=$(container image load --input "$OCI_TAR" 2>&1)
              LOADED_REF=$(echo "$LOAD_OUTPUT" | grep -oE 'untagged@sha256:[a-f0-9]+' | head -1)
              if [ -n "$LOADED_REF" ]; then
                container image tag "$LOADED_REF" "$PROFILE_IMAGE"
              fi
              rm -f "$OCI_TAR"
              verbose "Loaded image $PROFILE_IMAGE"
            else
              verbose "Using cached image $PROFILE_IMAGE"
            fi

            verbose "Project dir: $PROJECT_DIR"

            # Build mount arguments at runtime
            # VirtioFS quirks we handle:
            #   1. Files appear as root-owned - entrypoint copies with correct ownership
            #   2. Only directory mounts work - files mounted via parent directory
            #   3. Symlinks pointing outside mount are broken - dereference on host
            #   4. Sockets mount with 0000 perms - entrypoint runs chmod to fix
            #
            # Security: We dereference symlinks on the HOST to avoid mounting /nix/store
            # Use PID-based staging to allow multiple concurrent containers
            ${cleanStaleStagingDirs}
            ${createStagingDir}

            ${expandPathFn}

            # Read git author from host config (overrideable via env vars)
            GIT_AUTHOR_NAME="''${GIT_AUTHOR_NAME:-$(git config --global user.name 2>/dev/null || echo 'Wrapix Sandbox')}"
            GIT_AUTHOR_EMAIL="''${GIT_AUTHOR_EMAIL:-$(git config --global user.email 2>/dev/null || echo 'sandbox@wrapix.dev')}"
            GIT_COMMITTER_NAME="''${GIT_COMMITTER_NAME:-$GIT_AUTHOR_NAME}"
            GIT_COMMITTER_EMAIL="''${GIT_COMMITTER_EMAIL:-$GIT_AUTHOR_EMAIL}"

            MOUNT_ARGS=""
            DIR_MOUNTS=""
            FILE_MOUNTS=""
            SOCK_MOUNTS=""
            MOUNTED_FILE_DIRS=""
            dir_idx=0
            file_idx=0

            while IFS=: read -r src dest optional; do
              [ -z "$src" ] && continue
              src=$(expand_path "$src")
              dest=$(expand_path "$dest")

              if [ ! -e "$src" ]; then
                [ "$optional" = "optional" ] && continue
                echo "Error: Mount source not found: $src"
                exit 1
              fi

              if [ -d "$src" ]; then
                # Dereference symlinks on host to avoid mounting /nix/store
                host_staging="$STAGING_ROOT/dir$dir_idx"
                mkdir -p "$host_staging"
                cp -rL "$src/." "$host_staging/"

                staging="/mnt/wrapix/dir$dir_idx"
                dir_idx=$((dir_idx + 1))
                MOUNT_ARGS="$MOUNT_ARGS -v $host_staging:$staging"
                [ -n "$DIR_MOUNTS" ] && DIR_MOUNTS="$DIR_MOUNTS,"
                DIR_MOUNTS="$DIR_MOUNTS$staging:$dest"
              elif [ -S "$src" ]; then
                # Socket: mount directly, fix permissions in entrypoint
                # VirtioFS may show wrong permissions (0000) but chmod can fix it
                MOUNT_ARGS="$MOUNT_ARGS -v $src:$dest"
                [ -n "$SOCK_MOUNTS" ] && SOCK_MOUNTS="$SOCK_MOUNTS,"
                SOCK_MOUNTS="$SOCK_MOUNTS$dest"
              else
                # File: mount parent dir to staging (dedup), track for entrypoint to copy
                parent_dir=$(dirname "$src")
                file_name=$(basename "$src")
                # Check if parent already mounted
                staging=""
                for entry in $MOUNTED_FILE_DIRS; do
                  dir="''${entry%%=*}"
                  path="''${entry#*=}"
                  if [ "$dir" = "$parent_dir" ]; then
                    staging="$path"
                    break
                  fi
                done
                if [ -z "$staging" ]; then
                  staging="/mnt/wrapix/file$file_idx"
                  file_idx=$((file_idx + 1))
                  MOUNT_ARGS="$MOUNT_ARGS -v $parent_dir:$staging"
                  MOUNTED_FILE_DIRS="$MOUNTED_FILE_DIRS $parent_dir=$staging"
                fi
                [ -n "$FILE_MOUNTS" ] && FILE_MOUNTS="$FILE_MOUNTS,"
                FILE_MOUNTS="$FILE_MOUNTS$staging/$file_name:$dest"
              fi
            done <<'MOUNTS'
      ${mkMountSpecs {
        inherit profile;
        includeMode = false;
      }}
      MOUNTS

            # Add SSH known_hosts and system prompt (directories from Nix store)
            # Note: prompt mounted to /etc/wrapix-prompts (not /etc/wrapix) to preserve
            # claude-config.json and claude-settings.json baked into /etc/wrapix by image.nix
            MOUNT_ARGS="$MOUNT_ARGS -v ${knownHosts}:${sshConfig.knownHostsDirTarget}"
            MOUNT_ARGS="$MOUNT_ARGS -v ${promptDir}:/etc/wrapix-prompts"

            # Notifications use TCP to gateway (port 5959) instead of mounted Unix socket
            # VirtioFS cannot pass Unix socket operations, so the container client
            # connects to the host daemon via TCP (WRAPIX_NOTIFY_TCP=1 set below)

            # Add deploy key and signing key (not under ~/.ssh/ — see lib/util/ssh.nix)
            DEPLOY_KEY_NAME=${deployKeyExpr}
            DEPLOY_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME"
            SIGNING_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing"
            DEPLOY_KEY_ARGS=""
            if [ -f "$DEPLOY_KEY" ]; then
              DEPLOY_STAGING="$STAGING_ROOT/deploy_keys"
              mkdir -p "$DEPLOY_STAGING"
              cp "$DEPLOY_KEY" "$DEPLOY_STAGING/$DEPLOY_KEY_NAME"
              [ -f "$SIGNING_KEY" ] && cp "$SIGNING_KEY" "$DEPLOY_STAGING/$DEPLOY_KEY_NAME-signing"
              MOUNT_ARGS="$MOUNT_ARGS -v $DEPLOY_STAGING:/mnt/wrapix/deploy_keys"
              [ -n "$FILE_MOUNTS" ] && FILE_MOUNTS="$FILE_MOUNTS,"
              FILE_MOUNTS="$FILE_MOUNTS/mnt/wrapix/deploy_keys/$DEPLOY_KEY_NAME:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
              DEPLOY_KEY_ARGS="-e WRAPIX_DEPLOY_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
            fi
            if [ -f "$SIGNING_KEY" ]; then
              [ -n "$FILE_MOUNTS" ] && FILE_MOUNTS="$FILE_MOUNTS,"
              FILE_MOUNTS="$FILE_MOUNTS/mnt/wrapix/deploy_keys/$DEPLOY_KEY_NAME-signing:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
              DEPLOY_KEY_ARGS="$DEPLOY_KEY_ARGS -e WRAPIX_SIGNING_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
            fi

            ${stageBeads}
            if [ -n "$BEADS_STAGING" ]; then
              MOUNT_ARGS="$MOUNT_ARGS -v $BEADS_STAGING:/workspace/.beads"
            fi

            # Session registration for focus-aware notifications (tmux only)
            WRAPIX_SESSION_ID=""
            WRAPIX_SESSION_FILE=""
            if [ -n "''${TMUX:-}" ]; then
              WRAPIX_SESSION_ID=$(tmux display-message -p '#S:#I.#P')
              WRAPIX_SESSION_DIR="''${XDG_DATA_HOME:-$HOME/.local/share}/wrapix/sessions"
              mkdir -p "$WRAPIX_SESSION_DIR"

              # Capture terminal app name (no sudo required, may need Accessibility permission)
              TERMINAL_APP=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null || echo "")

              # Use safe filename (replace : and . with -)
              SAFE_SESSION_ID="''${WRAPIX_SESSION_ID//[:\.]/-}"
              WRAPIX_SESSION_FILE="$WRAPIX_SESSION_DIR/$SAFE_SESSION_ID.json"
              printf '{"session_id":"%s","terminal_app":"%s"}\n' "$WRAPIX_SESSION_ID" "$TERMINAL_APP" > "$WRAPIX_SESSION_FILE"
            fi

            # Cleanup function for session file
            cleanup_session() {
              [ -n "$WRAPIX_SESSION_FILE" ] && [ -f "$WRAPIX_SESSION_FILE" ] && rm -f "$WRAPIX_SESSION_FILE"
            }
            trap cleanup_session EXIT

            # Validate WRAPIX_NETWORK mode (default: open)
            WRAPIX_NETWORK="''${WRAPIX_NETWORK:-open}"
            case "$WRAPIX_NETWORK" in
              open|limit) ;;
              *)
                echo "Error: WRAPIX_NETWORK must be 'open' or 'limit' (got: $WRAPIX_NETWORK)" >&2
                exit 1
                ;;
            esac

            # Build environment arguments (use array to handle spaces in values)
            ENV_ARGS=()
            ENV_ARGS+=(-e "WRAPIX_VERBOSE=''${WRAPIX_VERBOSE:-}")
            ENV_ARGS+=(-e "BD_NO_DAEMON=1")
            ENV_ARGS+=(-e "CLAUDE_CODE_OAUTH_TOKEN=''${CLAUDE_CODE_OAUTH_TOKEN:-}")
            ENV_ARGS+=(-e "RALPH_MODE=''${RALPH_MODE:-}")
            ENV_ARGS+=(-e "RALPH_CMD=''${RALPH_CMD:-}")
            ENV_ARGS+=(-e "RALPH_ARGS=''${RALPH_ARGS:-}")
            ENV_ARGS+=(-e "RALPH_DIR=''${RALPH_DIR:-}")
            ENV_ARGS+=(-e "RALPH_DEBUG=''${RALPH_DEBUG:-}")
            ENV_ARGS+=(-e "HOST_UID=$(id -u)")
            ENV_ARGS+=(-e "GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME")
            ENV_ARGS+=(-e "GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL")
            ENV_ARGS+=(-e "GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME")
            ENV_ARGS+=(-e "GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL")
            [ -n "''${WRAPIX_GIT_SIGN:-}" ] && ENV_ARGS+=(-e "WRAPIX_GIT_SIGN=$WRAPIX_GIT_SIGN")
            [ -n "$DIR_MOUNTS" ] && ENV_ARGS+=(-e "WRAPIX_DIR_MOUNTS=$DIR_MOUNTS")
            [ -n "$FILE_MOUNTS" ] && ENV_ARGS+=(-e "WRAPIX_FILE_MOUNTS=$FILE_MOUNTS")
            [ -n "$SOCK_MOUNTS" ] && ENV_ARGS+=(-e "WRAPIX_SOCK_MOUNTS=$SOCK_MOUNTS")
            # Tell notification client to use TCP (VirtioFS can't pass Unix sockets)
            ENV_ARGS+=(-e "WRAPIX_NOTIFY_TCP=1")
            # Pass session ID for focus-aware notifications (empty if not in tmux)
            ENV_ARGS+=(-e "WRAPIX_SESSION_ID=$WRAPIX_SESSION_ID")
            # Pass network mode and allowlist for WRAPIX_NETWORK=limit filtering
            ENV_ARGS+=(-e "WRAPIX_NETWORK=$WRAPIX_NETWORK")
            ENV_ARGS+=(-e "WRAPIX_NETWORK_ALLOWLIST=${networkAllowlist}")

            # Generate unique container name
            CONTAINER_NAME="wrapix-$$"

            # Calculate CPUs (use override or half of available, minimum 2)
            ${
              if cpus != null then
                ''
                  CPUS=${toString cpus}
                ''
              else
                ''
                  CPUS=$(($(sysctl -n hw.ncpu) / 2))
                  [ "$CPUS" -lt 2 ] && CPUS=2
                ''
            }

            # Ensure .claude directory exists on host for session persistence
            mkdir -p "$PROJECT_DIR/.claude"

            verbose "Starting container (cpus=$CPUS, memory=${toString memoryMb}M)..."

            # Run container
            # Note: -w / because WorkingDir=/workspace fails before mounts are ready
            # Note: ~/.claude is NOT mounted from host — the entrypoint selectively
            # symlinks persistent items from /workspace/.claude instead. This keeps
            # user-level settings.json separate from project-level settings.json,
            # avoiding Claude Code writing user-only properties (like
            # skipDangerousModePermissionPrompt) to the project settings path.
            TTY_ARGS=""
            [ -t 0 ] && TTY_ARGS="-t -i"

            exec container run \
              --name "$CONTAINER_NAME" \
              --rm \
              $TTY_ARGS \
              -w / \
              -c "$CPUS" \
              -m ${toString memoryMb}M \
              --network default \
              --dns 100.100.100.100 \
              --dns 1.1.1.1 \
              -v "$PROJECT_DIR:/workspace" \
              $MOUNT_ARGS \
              "''${ENV_ARGS[@]}" \
              $DEPLOY_KEY_ARGS \
              -- \
              "''${WRAPIX_IMAGE:-$PROFILE_IMAGE}" \
              "''${CONTAINER_CMD[@]}"
    '';
}
