# Linux sandbox implementation using a single container
{ pkgs }:

let
  inherit (builtins) readFile;
  inherit (paths) mkMountSpecs;
  inherit (pkgs) writeShellApplication writeText;
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

  prompt = writeText "wrapix-prompt" (readFile ../prompt.txt);

  # crun built with libkrun support for microVM boundary
  # Provides 'krun' binary (symlink to crun) for podman --runtime krun
  # libkrun is dlopen'd at runtime, so we patch RPATH to find it
  crun-krun = pkgs.crun.overrideAttrs (old: {
    pname = "crun-krun";
    buildInputs = old.buildInputs ++ [ pkgs.libkrun ];
    configureFlags = (old.configureFlags or [ ]) ++ [ "--with-libkrun" ];
    postFixup = (old.postFixup or "") + ''
      patchelf --add-rpath ${pkgs.lib.getLib pkgs.libkrun}/lib $out/bin/crun
    '';
  });

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
    writeShellApplication {
      name = "wrapix";
      runtimeInputs = [
        crun-krun
        pkgs.beads-dolt
        pkgs.podman
      ];
      text = ''
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

        ${cleanStaleStagingDirs}

        ${createStagingDir}

        ${expandPathFn}

        verbose "Project dir: $PROJECT_DIR"

        # Ensure the per-workspace wrapix-beads dolt container is running
        # before launching. Without it, bd inside the container has no
        # server to talk to and the socket at $PROJECT_DIR/.gc/dolt.sock
        # won't exist. beads-dolt start is idempotent.
        if [ -d "$PROJECT_DIR/.beads/dolt" ] && command -v podman >/dev/null 2>&1; then
          beads-dolt start "$PROJECT_DIR"
        fi

        # Read git author from host config (overrideable via env vars)
        GIT_AUTHOR_NAME="''${GIT_AUTHOR_NAME:-$(git config --global user.name 2>/dev/null || echo 'Wrapix Sandbox')}"
        GIT_AUTHOR_EMAIL="''${GIT_AUTHOR_EMAIL:-$(git config --global user.email 2>/dev/null || echo 'sandbox@wrapix.dev')}"
        GIT_COMMITTER_NAME="''${GIT_COMMITTER_NAME:-$GIT_AUTHOR_NAME}"
        GIT_COMMITTER_EMAIL="''${GIT_COMMITTER_EMAIL:-$GIT_AUTHOR_EMAIL}"

        # Build volume args
        VOLUME_ARGS="-v $PROJECT_DIR:/workspace:rw"

        # Ensure project .claude dir exists for session persistence (/resume, /rename)
        # ~/.claude is container-local (tmpfs); entrypoint symlinks persistent items
        mkdir -p "$PROJECT_DIR/.claude"

        dir_idx=0

        verbose "Staging profile mounts..."
        # Process profile mounts - stage directories to dereference symlinks
        while IFS=: read -r src dest mode optional; do
          [ -z "$src" ] && continue
          src=$(expand_path "$src")
          dest=$(expand_path "$dest")

          if [ ! -e "$src" ]; then
            [ "$optional" = "optional" ] && continue
            echo "Error: Mount source not found: $src"
            exit 1
          fi

          if [ -d "$src" ]; then
            # Stage directory with cp -rL to dereference symlinks (e.g., nix store)
            staging="$STAGING_ROOT/dir$dir_idx"
            mkdir -p "$staging"
            cp -rL "$src/." "$staging/"
            dir_idx=$((dir_idx + 1))
            VOLUME_ARGS="$VOLUME_ARGS -v $staging:$dest:$mode"
          else
            # Files can be mounted directly
            VOLUME_ARGS="$VOLUME_ARGS -v $src:$dest:$mode"
          fi
        done <<'MOUNTS'
        ${mkMountSpecs {
          inherit profile;
          includeMode = true;
        }}
        MOUNTS

        # Mount SSH known_hosts as system-wide file (not under ~/.ssh/ —
        # podman auto-creates parent dirs owned by root on the tmpfs home)
        VOLUME_ARGS="$VOLUME_ARGS -v ${knownHosts}/known_hosts:${sshConfig.knownHostsTarget}:ro"
        VOLUME_ARGS="$VOLUME_ARGS -v ${prompt}:/etc/wrapix-prompt:ro"

        # Mount notification socket directory if daemon is running
        # We mount the directory (not the socket file) so daemon restarts work
        # without needing to restart the container
        NOTIFY_SOCKET_DIR="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrapix"
        if [ -S "$NOTIFY_SOCKET_DIR/notify.sock" ]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $NOTIFY_SOCKET_DIR:/run/wrapix"
        else
          echo "Note: Notification socket not found at $NOTIFY_SOCKET_DIR/notify.sock" >&2
          echo "      Run 'nix run .#wrapix-notifyd' on host for desktop notifications" >&2
        fi

        # Mount host podman socket for sibling container access (opt-in)
        PODMAN_SOCKET_ARGS=""
        if [ -n "''${WRAPIX_PODMAN_SOCKET:-}" ]; then
          PODMAN_SOCK="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/podman/podman.sock"
          if [ -S "$PODMAN_SOCK" ]; then
            PODMAN_SOCKET_ARGS="-v $PODMAN_SOCK:/run/podman/podman.sock -e CONTAINER_HOST=unix:///run/podman/podman.sock"
            # Tell nested podman commands where the host sees /workspace.
            # $PROJECT_DIR is the host path (the launcher runs on the host).
            PODMAN_SOCKET_ARGS="$PODMAN_SOCKET_ARGS -e GC_HOST_WORKSPACE=$PROJECT_DIR"
          else
            echo "Error: WRAPIX_PODMAN_SOCKET set but socket not found at $PODMAN_SOCK" >&2
            exit 1
          fi
        fi

        # Mount deploy key and signing key (not under ~/.ssh/ — see lib/util/ssh.nix)
        DEPLOY_KEY_NAME=${deployKeyExpr}
        DEPLOY_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME"
        SIGNING_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing"
        DEPLOY_KEY_ARGS=""
        if [ -f "$DEPLOY_KEY" ]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $DEPLOY_KEY:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME:ro"
          DEPLOY_KEY_ARGS="-e WRAPIX_DEPLOY_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
        fi
        if [ -f "$SIGNING_KEY" ]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $SIGNING_KEY:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing:ro"
          DEPLOY_KEY_ARGS="$DEPLOY_KEY_ARGS -e WRAPIX_SIGNING_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
        fi

        ${stageBeads}
        BEADS_ARGS=""
        if [ -n "$BEADS_STAGING" ]; then
          BEADS_ARGS="-v $BEADS_STAGING:/workspace/.beads"
          if [ -n "''${WRAPIX_PODMAN_SOCKET:-}" ]; then
            PODMAN_SOCKET_ARGS="$PODMAN_SOCKET_ARGS -e GC_HOST_BEADS=$BEADS_STAGING"
          fi
        fi

        # Session registration for focus-aware notifications (tmux only)
        WRAPIX_SESSION_ID=""
        WRAPIX_SESSION_FILE=""
        if [ -n "''${TMUX:-}" ]; then
          WRAPIX_SESSION_ID=$(tmux display-message -p '#S:#I.#P')
          WRAPIX_SESSION_DIR="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrapix/sessions"
          mkdir -p "$WRAPIX_SESSION_DIR"

          # Capture window ID for focus detection (niri-specific)
          WINDOW_ID=""
          if command -v niri >/dev/null 2>&1; then
            WINDOW_ID=$(niri msg -j focused-window 2>/dev/null | jq -r '.id // ""') || WINDOW_ID=""
          fi

          # Use safe filename (replace : and . with -)
          SAFE_SESSION_ID="''${WRAPIX_SESSION_ID//[:\.]/-}"
          WRAPIX_SESSION_FILE="$WRAPIX_SESSION_DIR/$SAFE_SESSION_ID.json"
          printf '{"session_id":"%s","window_id":"%s"}\n' "$WRAPIX_SESSION_ID" "$WINDOW_ID" > "$WRAPIX_SESSION_FILE"
        fi

        # Cleanup function for session file
        # shellcheck disable=SC2329 # Invoked via trap
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

        # Network filtering requires NET_ADMIN capability for iptables
        NETWORK_CAP_ARGS=""
        if [ "$WRAPIX_NETWORK" = "limit" ]; then
          NETWORK_CAP_ARGS="--cap-add=NET_ADMIN"
        fi

        # Calculate CPUs (use override or half of available, minimum 2)
        ${
          if cpus != null then
            ''
              CPUS=${toString cpus}
            ''
          else
            ''
              CPUS=$(($(nproc) / 2))
              [ "$CPUS" -lt 2 ] && CPUS=2
            ''
        }

        # Load image using hash-based tag — no version file needed.
        # The tag is derived from the Nix store path of the image
        # derivation, which changes when any input changes.
        IMAGE_ID="localhost/wrapix-${profile.name}:${imageTagLib.mkImageTag profileImage}"
        if ! podman image exists "$IMAGE_ID" 2>/dev/null; then
          verbose "Loading image from ${profileImage}..."
          ${profileImage} | podman load -q >/dev/null
          podman tag "localhost/wrapix-${profile.name}:latest" "$IMAGE_ID" 2>/dev/null || true
          podman images --filter "reference=localhost/wrapix-${profile.name}" --format '{{.Tag}}' | while read -r _old_tag; do
            case "$_old_tag" in latest|${imageTagLib.mkImageTag profileImage}) continue ;; esac
            podman rmi "localhost/wrapix-${profile.name}:$_old_tag" 2>/dev/null || true
          done
          verbose "Loaded image $IMAGE_ID"
        else
          verbose "Using cached image $IMAGE_ID"
        fi

        verbose "Starting container (cpus=$CPUS, memory=${toString memoryMb}m)..."

        # Detect krun availability for microVM boundary (see specs/security-review.md)
        # Default: container boundary (krun microVM currently disabled)
        # WRAPIX_MICROVM=1: explicit opt-in to microVM boundary
        RUNTIME_ARGS=""
        if [ "''${WRAPIX_MICROVM:-}" = "1" ]; then
          if ! [ -e /dev/kvm ]; then
            echo "Error: /dev/kvm not found. A microVM boundary requires KVM support." >&2
            exit 1
          elif ! command -v krun >/dev/null 2>&1 && ! podman info --format '{{range .Host.OCIRuntime.Alternatives}}{{.}}{{end}}' 2>/dev/null | grep -q krun; then
            echo "Error: krun runtime not found. A microVM boundary requires crun with libkrun." >&2
            exit 1
          else
            RUNTIME_ARGS="--runtime krun"
          fi
        fi

        # krun microVM: PTY relay + LD_PRELOAD for root UID
        #
        # krun's virtio console doesn't support raw mode (TCSETS may silently
        # fail or be ignored). krun-relay creates a real PTY inside the VM
        # where raw mode works, and relays I/O between console and PTY.
        # It also converts \n→\r on input (console ICRNL converts Enter's
        # \r to \n; Claude expects \r for submit).
        #
        # libfakeuid.so handles UID spoofing (krun maps host user to root,
        # claude refuses root) and TIOCGWINSZ fallback.
        KRUN_ENTRYPOINT_ARGS=""
        KRUN_ENV_ARGS=""
        KRUN_CMD_ENV=""
        if [ -n "$RUNTIME_ARGS" ]; then
          # Capture host terminal dimensions for PTY sizing
          TERM_ROWS=$(stty size 2>/dev/null | awk '{print $1}') || true
          TERM_COLS=$(stty size 2>/dev/null | awk '{print $2}') || true
          : "''${TERM_ROWS:=24}" "''${TERM_COLS:=80}"

          # krun-relay as PID 1: creates real PTY, relays I/O, execs krun-init.sh
          KRUN_ENTRYPOINT_ARGS="--entrypoint /krun-relay"
          KRUN_ENV_ARGS="-e WRAPIX_TERM_ROWS=$TERM_ROWS -e WRAPIX_TERM_COLS=$TERM_COLS"

          # Serialize container command for krun-init.sh (preserves quoting)
          # Kept separate from KRUN_ENV_ARGS to avoid word-splitting the value
          if [ ''${#CONTAINER_CMD[@]} -gt 0 ]; then
            KRUN_CMD_ENV="WRAPIX_KRUN_CMD=$(printf '%q ' "''${CONTAINER_CMD[@]}")"
          fi

          # krun-relay execs /krun-init.sh which handles args via WRAPIX_KRUN_CMD
          CONTAINER_CMD=()
        fi

        # shellcheck disable=SC2086 # Intentional word splitting for volume args
        exec podman run --rm -it \
          $RUNTIME_ARGS \
          $KRUN_ENTRYPOINT_ARGS \
          $NETWORK_CAP_ARGS \
          --cpus="$CPUS" \
          --memory=${toString memoryMb}m \
          --pids-limit=4096 \
          --network=pasta \
          --userns=keep-id \
          --passwd-entry "wrapix:*:$(id -u):$(id -g)::/home/wrapix:/bin/bash" \
          --mount type=tmpfs,destination=/home/wrapix,U=true \
          $VOLUME_ARGS \
          $BEADS_ARGS \
          $DEPLOY_KEY_ARGS \
          $PODMAN_SOCKET_ARGS \
          $KRUN_ENV_ARGS \
          ''${KRUN_CMD_ENV:+-e "$KRUN_CMD_ENV"} \
          -e "BD_NO_DAEMON=1" \
          -e "CLAUDE_CODE_OAUTH_TOKEN=''${CLAUDE_CODE_OAUTH_TOKEN:-}" \
          -e "RALPH_MODE=''${RALPH_MODE:-}" \
          -e "RALPH_CMD=''${RALPH_CMD:-}" \
          -e "RALPH_ARGS=''${RALPH_ARGS:-}" \
          -e "RALPH_DIR=''${RALPH_DIR:-}" \
          -e "RALPH_DEBUG=''${RALPH_DEBUG:-}" \
          -e "HOME=/home/wrapix" \
          -e "GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME" \
          -e "GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL" \
          -e "GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME" \
          -e "GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL" \
          -e "WRAPIX_SESSION_ID=$WRAPIX_SESSION_ID" \
          -e "WRAPIX_VERBOSE=''${WRAPIX_VERBOSE:-}" \
          -e "WRAPIX_NETWORK=$WRAPIX_NETWORK" \
          -e "WRAPIX_NETWORK_ALLOWLIST=${networkAllowlist}" \
          ''${WRAPIX_GIT_SIGN:+-e "WRAPIX_GIT_SIGN=$WRAPIX_GIT_SIGN"} \
          -w /workspace \
          "$IMAGE_ID" \
          "''${CONTAINER_CMD[@]}"
      '';
    };
}
