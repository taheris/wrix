# Darwin sandbox using Apple container CLI (macOS 26+)
{ pkgs }:

let
  inherit (builtins) readFile;
  inherit (paths) mkMountSpecs;
  inherit (pkgs) writeShellScriptBin writeTextDir;
  inherit (shellLib)
    expandPathFn
    fixVmnetRoute
    cleanStaleStagingDirs
    createStagingDir
    mkDeployKeyExpr
    pruneStaleImages
    stageBeads
    ;

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

            # Subcommand dispatch: `wrapix run` (interactive, TTY) vs
            # `wrapix spawn` (stdio, JSON spawn-config).
            SUBCOMMAND="run"
            if [ $# -gt 0 ]; then
              case "$1" in
                run|spawn) SUBCOMMAND="$1"; shift ;;
              esac
            fi

            SPAWN_CONFIG=""
            USE_STDIO=0
            IMAGE_OVERRIDE_REF=""
            IMAGE_OVERRIDE_SOURCE=""
            IMAGE_OVERRIDE_DIGEST=""
            CONTAINER_CMD=()
            SPAWN_ENV=()
            SPAWN_MOUNTS=()

            if [ "$SUBCOMMAND" = "spawn" ]; then
              while [ $# -gt 0 ]; do
                case "$1" in
                  --spawn-config)
                    [ $# -lt 2 ] && { echo "Error: --spawn-config requires <file>" >&2; exit 2; }
                    SPAWN_CONFIG="$2"; shift 2 ;;
                  --stdio) USE_STDIO=1; shift ;;
                  --) shift; break ;;
                  *) echo "Error: unknown wrapix spawn flag: $1" >&2; exit 2 ;;
                esac
              done
              if [ -z "$SPAWN_CONFIG" ]; then
                echo "Error: wrapix spawn requires --spawn-config <file>" >&2
                exit 2
              fi
              if [ ! -f "$SPAWN_CONFIG" ]; then
                echo "Error: spawn-config file not found: $SPAWN_CONFIG" >&2
                exit 1
              fi
              # Stable JSON shape (the loom repo SpawnConfig): image_ref,
              # image_source, workspace, env, initial_prompt, agent_args, repin.
              PROJECT_DIR=$(${pkgs.jq}/bin/jq -r '.workspace' "$SPAWN_CONFIG")
              # `// ""` coerces missing keys / explicit nulls to empty strings,
              # so the load-step gate (`-n "$IMAGE_SOURCE"`) skips cleanly when
              # the orchestrator provides only image_ref.
              IMAGE_OVERRIDE_REF=$(${pkgs.jq}/bin/jq -r '.image_ref // ""' "$SPAWN_CONFIG")
              IMAGE_OVERRIDE_SOURCE=$(${pkgs.jq}/bin/jq -r '.image_source // ""' "$SPAWN_CONFIG")
              IMAGE_OVERRIDE_DIGEST=$(${pkgs.jq}/bin/jq -r '.image_digest_path // ""' "$SPAWN_CONFIG")
              while IFS= read -r pair; do
                [ -z "$pair" ] && continue
                SPAWN_ENV+=("$pair")
              done < <(${pkgs.jq}/bin/jq -r '.env[]? | "\(.[0])=\(.[1])"' "$SPAWN_CONFIG")
              while IFS= read -r arg; do
                CONTAINER_CMD+=("$arg")
              done < <(${pkgs.jq}/bin/jq -r '.agent_args[]?' "$SPAWN_CONFIG")
              # SpawnConfig.mounts: tab-separated host_path, container_path,
              # read_only. Missing/empty list yields zero entries (matches the
              # loom-side `#[serde(default, skip_serializing_if = ...)]`).
              while IFS= read -r entry; do
                [ -z "$entry" ] && continue
                SPAWN_MOUNTS+=("$entry")
              done < <(${pkgs.jq}/bin/jq -r '.mounts[]? | [.host_path, .container_path, (.read_only|tostring)] | @tsv' "$SPAWN_CONFIG")
            else
              PROJECT_DIR="''${1:-$(pwd)}"
              shift || true
              # Remaining args override the container command (passed to entrypoint as $@)
              if [ $# -gt 0 ]; then
                CONTAINER_CMD=("$@")
              fi
            fi

            # WRAPIX_DRY_RUN=1: print resolved spawn state, run the mount
            # classifier with filesystem ops disabled, dump classified mount
            # intents, and exit before any container CLI invocation. Used by
            # tests to verify SpawnConfig parsing and mount classification
            # without a runtime.
            if [ "''${WRAPIX_DRY_RUN:-}" = "1" ]; then
              printf 'SUBCOMMAND=%s\n' "$SUBCOMMAND"
              printf 'STDIO=%s\n' "$USE_STDIO"
              printf 'WORKSPACE=%s\n' "$PROJECT_DIR"
              printf 'IMAGE_OVERRIDE_REF=%s\n' "$IMAGE_OVERRIDE_REF"
              printf 'IMAGE_OVERRIDE_SOURCE=%s\n' "$IMAGE_OVERRIDE_SOURCE"
              printf 'IMAGE_OVERRIDE_DIGEST=%s\n' "$IMAGE_OVERRIDE_DIGEST"
              for pair in "''${SPAWN_ENV[@]}"; do printf 'ENV=%s\n' "$pair"; done
              for arg in "''${CONTAINER_CMD[@]}"; do printf 'CMD=%s\n' "$arg"; done
              for entry in "''${SPAWN_MOUNTS[@]}"; do printf 'MOUNT=%s\n' "$entry"; done
            fi

            if [ "''${WRAPIX_DRY_RUN:-}" != "1" ]; then
              # Check macOS version
              if [ "$(sw_vers -productVersion | cut -d. -f1)" -lt 26 ]; then
                echo "Error: macOS 26+ required (current: $(sw_vers -productVersion))"
                exit 1
              fi

              # Ensure container system is running
              if ! container system status >/dev/null 2>&1; then
                echo "Starting container system..." >&2
                container system start
                sleep 2
              fi

              # Ensure the per-workspace wrapix-beads dolt container is running.
              # Uses Apple container CLI (same runtime as the sandbox itself).
              if [ -d "$PROJECT_DIR/.beads/dolt" ]; then
                ${pkgs.beads-dolt}/bin/beads-dolt start "$PROJECT_DIR"
              fi

              ${fixVmnetRoute}
            fi

            # Image is supplied to the launcher at runtime, not baked in. For
            # `wrapix run`, $WRAPIX_DEFAULT_IMAGE_REF and $WRAPIX_DEFAULT_IMAGE_SOURCE
            # are set by the per-profile makeWrapper composition (or by an
            # orchestrator like loom). For `wrapix spawn`, the SpawnConfig
            # carries `image_ref` and `image_source`.
            IMAGE_REF=""
            IMAGE_SOURCE=""
            IMAGE_DIGEST_PATH=""
            if [ "$SUBCOMMAND" = "run" ]; then
              if [ -z "''${WRAPIX_DEFAULT_IMAGE_REF:-}" ] || [ -z "''${WRAPIX_DEFAULT_IMAGE_SOURCE:-}" ]; then
                echo "Error: wrapix run requires WRAPIX_DEFAULT_IMAGE_REF and WRAPIX_DEFAULT_IMAGE_SOURCE" >&2
                exit 1
              fi
              IMAGE_REF="$WRAPIX_DEFAULT_IMAGE_REF"
              IMAGE_SOURCE="$WRAPIX_DEFAULT_IMAGE_SOURCE"
              IMAGE_DIGEST_PATH="''${WRAPIX_DEFAULT_IMAGE_DIGEST:-}"
            else
              IMAGE_REF="$IMAGE_OVERRIDE_REF"
              IMAGE_SOURCE="$IMAGE_OVERRIDE_SOURCE"
              IMAGE_DIGEST_PATH="$IMAGE_OVERRIDE_DIGEST"
            fi

            PROFILE_IMAGE="$IMAGE_REF"
            IMAGE_REPO="''${IMAGE_REF%:*}"
            if [ "''${WRAPIX_DRY_RUN:-}" != "1" ]; then
              if [ -z "$IMAGE_SOURCE" ]; then
                verbose "Using cached image $PROFILE_IMAGE"
              else
                # Content-digest preflight (specs/sandbox.md § Image install path):
                # short-circuit the load pipeline when an image with matching OCI
                # config digest is already in the platform store under any tag.
                # Apple's container CLI exposes no digest-as-ref lookup, so we
                # enumerate wrapix-* refs and compare each ref's content digest.
                # On a hit, the requested ref is aliased to the matching content
                # and no tar bytes are streamed, no skopeo conversion, no
                # `container image load` is invoked. Falls back to ref-existence
                # when IMAGE_DIGEST_PATH is empty (legacy spawn callers).
                _wrapix_skip_load=0
                _wrapix_desired_digest=""
                if [ -n "''${IMAGE_DIGEST_PATH:-}" ] && [ -s "$IMAGE_DIGEST_PATH" ]; then
                  _wrapix_desired_digest=$(cat "$IMAGE_DIGEST_PATH")
                fi

                if [ -n "$_wrapix_desired_digest" ]; then
                  _wrapix_desired_short="''${_wrapix_desired_digest#sha256:}"
                  _wrapix_match_ref=""
                  while IFS= read -r _ref; do
                    [ -z "$_ref" ] && continue
                    _wrapix_actual=$(container image inspect "$_ref" 2>/dev/null | ${pkgs.jq}/bin/jq -r '.[0].digest // .[0].id // empty')
                    _wrapix_actual_short="''${_wrapix_actual#sha256:}"
                    if [ -n "$_wrapix_actual_short" ] && [ "$_wrapix_actual_short" = "$_wrapix_desired_short" ]; then
                      _wrapix_match_ref="$_ref"
                      break
                    fi
                  done < <(container image list 2>/dev/null | tail -n +2 | awk '/^wrapix-/ {print $1 ":" $2}')

                  if [ -n "$_wrapix_match_ref" ]; then
                    # best-effort: requested ref may already alias matching content,
                    # in which case tag exits non-zero benignly; tar bytes still
                    # aren't streamed.
                    container image tag "$_wrapix_match_ref" "$PROFILE_IMAGE" 2>/dev/null || true
                    _wrapix_skip_load=1
                  fi
                elif container image inspect "$PROFILE_IMAGE" >/dev/null 2>&1; then
                  _wrapix_skip_load=1
                fi

                if [ "$_wrapix_skip_load" = "1" ]; then
                  verbose "Using cached image $PROFILE_IMAGE"
                else
                  verbose "Image hash changed or missing, reloading..."
                  echo "Loading profile image..."
                  # Drop the prior :latest alias so the new load can claim the same
                  # repo name; pruneStaleImages later cleans residual hash tags.
                  container image delete "$IMAGE_REPO:latest" 2>/dev/null || true
                  # Convert Docker-format tar to OCI-archive for Apple container CLI.
                  # --insecure-policy is safe: images are built locally from Nix
                  # derivations (trusted source with cryptographic hashes).
                  OCI_TAR="$WRAPIX_CACHE/profile-image-oci.tar"
                  mkdir -p "$WRAPIX_CACHE"
                  ${pkgs.skopeo}/bin/skopeo --insecure-policy copy --quiet "docker-archive:$IMAGE_SOURCE" "oci-archive:$OCI_TAR"
                  LOAD_OUTPUT=$(container image load --input "$OCI_TAR" 2>&1)
                  LOADED_REF=$(echo "$LOAD_OUTPUT" | grep -oE 'untagged@sha256:[a-f0-9]+' | head -1)
                  if [ -n "$LOADED_REF" ]; then
                    container image tag "$LOADED_REF" "$PROFILE_IMAGE"
                    # Maintain :latest as the keep-anchor for pruneStaleImages.
                    container image tag "$LOADED_REF" "$IMAGE_REPO:latest"
                  fi
                  rm -f "$OCI_TAR"
                  container image prune
                  verbose "Loaded image $PROFILE_IMAGE"
                fi
              fi
              # Prune runs on every invocation, not just after load, so stale
              # hashes from other profiles get swept even when the current
              # profile is cached.
              ${pruneStaleImages { runtime = "container"; }}
            fi

            verbose "Project dir: $PROJECT_DIR"

            # Build mount arguments at runtime
            # VirtioFS quirks we handle:
            #   1. Files appear as root-owned - entrypoint copies with correct ownership
            #   2. Only directory mounts work - files mounted via parent directory
            #   3. Symlinks pointing outside mount are broken - dereference on host
            # Unix-socket sources are rejected by the classifier (VirtioFS does
            # not pass socket operations), so no entrypoint-side socket chmod
            # is needed.
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
            MOUNTED_FILE_DIRS=""
            dir_idx=0
            file_idx=0

            # Unified classifier: profile.mounts (from mkMountSpecs at Nix-eval
            # time) and SpawnConfig.mounts (parsed from JSON at runtime) feed
            # the same branch logic. Socket sources are rejected loudly — see
            # specs/sandbox.md § Platform Implementations / macOS.
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
                host_staging="$STAGING_ROOT/dir$dir_idx"
                if [ "''${WRAPIX_DRY_RUN:-}" != "1" ]; then
                  mkdir -p "$host_staging"
                  cp -rL "$src/." "$host_staging/"
                fi

                staging="/mnt/wrapix/dir$dir_idx"
                dir_idx=$((dir_idx + 1))
                MOUNT_ARGS="$MOUNT_ARGS -v $host_staging:$staging"
                [ -n "$DIR_MOUNTS" ] && DIR_MOUNTS="$DIR_MOUNTS,"
                DIR_MOUNTS="$DIR_MOUNTS$staging:$dest"
              elif [ -S "$src" ]; then
                echo "wrapix: Unix-socket mount source rejected: $src -> $dest" >&2
                echo "  (VirtioFS does not pass socket operations; mounting would dead-end at connect())" >&2
                exit 1
              else
                parent_dir=$(dirname "$src")
                file_name=$(basename "$src")
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
            done < <(
              cat <<'PROFILE_MOUNTS'
      ${mkMountSpecs {
        inherit profile;
        includeMode = false;
      }}
      PROFILE_MOUNTS
              for entry in "''${SPAWN_MOUNTS[@]}"; do
                IFS=$'\t' read -r host container _ro <<<"$entry"
                printf '%s:%s:required\n' "$host" "$container"
              done
            )

            if [ "''${WRAPIX_DRY_RUN:-}" = "1" ]; then
              [ -n "$DIR_MOUNTS" ] && printf 'DIR_MOUNTS=%s\n' "$DIR_MOUNTS"
              [ -n "$FILE_MOUNTS" ] && printf 'FILE_MOUNTS=%s\n' "$FILE_MOUNTS"
              printf 'MOUNT_ARGS=%s\n' "$MOUNT_ARGS"
              exit 0
            fi

            # Add SSH known_hosts and system prompt (directories from Nix store)
            # Note: prompt mounted to /etc/wrapix-prompts (not /etc/wrapix) to preserve
            # claude-config.json and claude-settings.json baked into /etc/wrapix by image.nix
            MOUNT_ARGS="$MOUNT_ARGS -v ${knownHosts}:${sshConfig.knownHostsDirTarget}"
            MOUNT_ARGS="$MOUNT_ARGS -v ${promptDir}:/etc/wrapix-prompts"

            # Notifications use TCP to gateway (port 5959) instead of mounted Unix socket
            # VirtioFS cannot pass Unix socket operations, so the container client
            # connects to the host daemon via TCP (WRAPIX_NOTIFY_TCP=1 set below)

            # Add deploy key and signing key (not under ~/.ssh/ — see lib/util/ssh.nix).
            # Host-source resolution precedence per specs/security.md:
            #   1. $WRAPIX_{DEPLOY,SIGNING}_KEY pointing at an existing file.
            #   2. $HOME/.ssh/deploy_keys/<name>{,-signing} fallback.
            # Set-but-missing env is fail-loud (parent-process mistake).
            DEPLOY_KEY_NAME=${deployKeyExpr}
            DEPLOY_KEY=""
            if [ -n "''${WRAPIX_DEPLOY_KEY:-}" ]; then
              if [ ! -f "$WRAPIX_DEPLOY_KEY" ]; then
                echo "wrapix: WRAPIX_DEPLOY_KEY=$WRAPIX_DEPLOY_KEY: file does not exist" >&2
                exit 1
              fi
              DEPLOY_KEY="$WRAPIX_DEPLOY_KEY"
            elif [ -f "$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME" ]; then
              DEPLOY_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME"
            fi
            SIGNING_KEY=""
            if [ -n "''${WRAPIX_SIGNING_KEY:-}" ]; then
              if [ ! -f "$WRAPIX_SIGNING_KEY" ]; then
                echo "wrapix: WRAPIX_SIGNING_KEY=$WRAPIX_SIGNING_KEY: file does not exist" >&2
                exit 1
              fi
              SIGNING_KEY="$WRAPIX_SIGNING_KEY"
            elif [ -f "$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing" ]; then
              SIGNING_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing"
            fi
            DEPLOY_KEY_ARGS=""
            if [ -n "$DEPLOY_KEY" ] || [ -n "$SIGNING_KEY" ]; then
              DEPLOY_STAGING="$STAGING_ROOT/deploy_keys"
              mkdir -p "$DEPLOY_STAGING"
              MOUNT_ARGS="$MOUNT_ARGS -v $DEPLOY_STAGING:/mnt/wrapix/deploy_keys"
            fi
            if [ -n "$DEPLOY_KEY" ]; then
              cp "$DEPLOY_KEY" "$DEPLOY_STAGING/$DEPLOY_KEY_NAME"
              [ -n "$FILE_MOUNTS" ] && FILE_MOUNTS="$FILE_MOUNTS,"
              FILE_MOUNTS="$FILE_MOUNTS/mnt/wrapix/deploy_keys/$DEPLOY_KEY_NAME:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
              DEPLOY_KEY_ARGS="-e WRAPIX_DEPLOY_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
            fi
            if [ -n "$SIGNING_KEY" ]; then
              cp "$SIGNING_KEY" "$DEPLOY_STAGING/$DEPLOY_KEY_NAME-signing"
              [ -n "$FILE_MOUNTS" ] && FILE_MOUNTS="$FILE_MOUNTS,"
              FILE_MOUNTS="$FILE_MOUNTS/mnt/wrapix/deploy_keys/$DEPLOY_KEY_NAME-signing:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
              DEPLOY_KEY_ARGS="$DEPLOY_KEY_ARGS -e WRAPIX_SIGNING_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
            fi

            # spawn (loop agent) must sign and push: fail loud on a keyless boot, not at land-the-plane (specs/security.md § Deploy & Signing Keys).
            if [ "$SUBCOMMAND" = "spawn" ]; then
              if [ -z "$DEPLOY_KEY" ]; then
                echo "wrapix spawn: no deploy key resolved — set WRAPIX_DEPLOY_KEY to an existing file, or place one at $HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME" >&2
                exit 1
              fi
              if [ "''${WRAPIX_GIT_SIGN:-1}" != "0" ] && [ -z "$SIGNING_KEY" ]; then
                echo "wrapix spawn: no signing key resolved — set WRAPIX_SIGNING_KEY to an existing file, place one at $HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing, or set WRAPIX_GIT_SIGN=0 to disable commit signing" >&2
                exit 1
              fi
            fi

            ${stageBeads}
            if [ -n "$BEADS_STAGING" ]; then
              MOUNT_ARGS="$MOUNT_ARGS -v $BEADS_STAGING:/workspace/.beads"
            fi

            # VirtioFS can't pass Unix sockets. Dolt runs as an Apple Container
            # and publishes its port to all host interfaces including vmnet.
            # The sandbox container reaches dolt via the vmnet gateway IP.
            # No socat bridge needed — both containers are on the same vmnet.
            BEADS_DOLT_PORT=""
            if [ -d "$PROJECT_DIR/.beads/dolt" ]; then
              BEADS_DOLT_PORT=$(${pkgs.beads-dolt}/bin/beads-dolt port "$PROJECT_DIR")
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
            # In spawn mode env passthrough comes strictly from the
            # SpawnConfig allowlist; interactive run keeps the historical
            # host-env passthrough.
            ENV_ARGS=()
            if [ "$SUBCOMMAND" = "spawn" ]; then
              for pair in "''${SPAWN_ENV[@]}"; do
                ENV_ARGS+=(-e "$pair")
              done
            else
              ENV_ARGS+=(-e "WRAPIX_VERBOSE=''${WRAPIX_VERBOSE:-}")
              ENV_ARGS+=(-e "CLAUDE_CODE_OAUTH_TOKEN=''${CLAUDE_CODE_OAUTH_TOKEN:-}")
              [ -n "''${WRAPIX_GIT_SIGN:-}" ] && ENV_ARGS+=(-e "WRAPIX_GIT_SIGN=$WRAPIX_GIT_SIGN")
              ENV_ARGS+=(-e "WRAPIX_SESSION_ID=$WRAPIX_SESSION_ID")
            fi
            # Always-on container env: built from launcher state, not host passthrough.
            ENV_ARGS+=(-e "BD_NO_DAEMON=1")
            ENV_ARGS+=(-e "HOST_UID=$(id -u)")
            ENV_ARGS+=(-e "GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME")
            ENV_ARGS+=(-e "GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL")
            ENV_ARGS+=(-e "GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME")
            ENV_ARGS+=(-e "GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL")
            ENV_ARGS+=(-e "WRAPIX_AGENT=$WRAPIX_AGENT")
            [ -n "$DIR_MOUNTS" ] && ENV_ARGS+=(-e "WRAPIX_DIR_MOUNTS=$DIR_MOUNTS")
            [ -n "$FILE_MOUNTS" ] && ENV_ARGS+=(-e "WRAPIX_FILE_MOUNTS=$FILE_MOUNTS")
            # VirtioFS can't pass Unix sockets — use TCP for notifications and dolt.
            # Dolt is now also an Apple Container publishing to all interfaces;
            # the sandbox VM reaches it via the vmnet gateway.
            ENV_ARGS+=(-e "WRAPIX_NOTIFY_TCP=1")
            [ -n "$BEADS_DOLT_PORT" ] && ENV_ARGS+=(-e "BEADS_DOLT_SERVER_PORT=$BEADS_DOLT_PORT")
            [ -n "$BEADS_DOLT_PORT" ] && ENV_ARGS+=(-e "BEADS_DOLT_SERVER_HOST=192.168.64.1")
            # Pass network mode and allowlist for WRAPIX_NETWORK=limit filtering
            ENV_ARGS+=(-e "WRAPIX_NETWORK=$WRAPIX_NETWORK")
            [ "$_vpn_conflict" = true ] && ENV_ARGS+=(-e "WRAPIX_WAIT_FOR_ROUTE=1")
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
            if [ "$SUBCOMMAND" = "spawn" ]; then
              [ "$USE_STDIO" = "1" ] && TTY_ARGS="-i"
            else
              [ -t 0 ] && TTY_ARGS="-t -i"
            fi

            RUN_IMAGE="''${WRAPIX_IMAGE:-$PROFILE_IMAGE}"

            CONTAINER_EXIT=0
            container run \
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
              "$RUN_IMAGE" \
              "''${CONTAINER_CMD[@]}" \
              || CONTAINER_EXIT=$?
            exit $CONTAINER_EXIT
    '';
}
