# Linux sandbox implementation using a single container
{ pkgs }:

let
  inherit (builtins) concatStringsSep readFile;
  inherit (paths) mkMountSpecs;
  inherit (pkgs) writeShellApplication writeText;
  inherit (shellLib)
    expandPathFn
    cleanStaleStagingDirs
    createStagingDir
    imageLoadStep
    mkDeployKeyExpr
    pruneStaleImages
    stageBeads
    ;

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
        pkgs.jq
        pkgs.podman
        pkgs.skopeo
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

        # Subcommand dispatch: `wrapix run` (interactive, TTY) vs
        # `wrapix spawn` (stdio, JSON spawn-config). Default with no
        # subcommand keeps legacy positional invocation `wrapix [DIR] [CMD...]`.
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
        # SpawnConfig env allowlist: KEY=VALUE pairs (one per array slot)
        SPAWN_ENV=()
        # SpawnConfig per-launch mounts, pre-rendered as `host:container[:ro]`
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
          # Loom is the producer; we consume image_ref, image_source,
          # workspace, env, agent_args here. initial_prompt and repin are
          # consumed in-container by the agent (loom writes the files into
          # the workspace before spawn).
          PROJECT_DIR=$(jq -r '.workspace' "$SPAWN_CONFIG")
          # `// ""` coerces missing keys / explicit nulls to empty strings,
          # so the load-step gate (`-n "$IMAGE_SOURCE"`) skips cleanly when
          # the orchestrator provides only image_ref.
          IMAGE_OVERRIDE_REF=$(jq -r '.image_ref // ""' "$SPAWN_CONFIG")
          IMAGE_OVERRIDE_SOURCE=$(jq -r '.image_source // ""' "$SPAWN_CONFIG")
          IMAGE_OVERRIDE_DIGEST=$(jq -r '.image_digest_path // ""' "$SPAWN_CONFIG")
          while IFS= read -r pair; do
            [ -z "$pair" ] && continue
            SPAWN_ENV+=("$pair")
          done < <(jq -r '.env[]? | "\(.[0])=\(.[1])"' "$SPAWN_CONFIG")
          while IFS= read -r arg; do
            CONTAINER_CMD+=("$arg")
          done < <(jq -r '.agent_args[]?' "$SPAWN_CONFIG")
          # SpawnConfig.mounts: pre-render each entry to the podman volume
          # syntax `host:container[:ro]`. Missing / empty array yields zero
          # entries (matches the loom-side `#[serde(default,
          # skip_serializing_if = "Vec::is_empty")]` on SpawnConfig.mounts).
          while IFS= read -r entry; do
            [ -z "$entry" ] && continue
            SPAWN_MOUNTS+=("$entry")
          done < <(jq -r '.mounts[]? | "\(.host_path):\(.container_path)" + (if .read_only == true then ":ro" else "" end)' "$SPAWN_CONFIG")
        else
          PROJECT_DIR="''${1:-$(pwd)}"
          shift || true
          # Remaining args override the container command (passed to entrypoint as $@)
          if [ $# -gt 0 ]; then
            CONTAINER_CMD=("$@")
          fi
        fi

        # WRAPIX_DRY_RUN=1: print resolved spawn state and exit without
        # touching the filesystem or invoking podman. Used by tests to
        # verify SpawnConfig parsing and per-bead profile selection
        # without a container runtime.
        if [ "''${WRAPIX_DRY_RUN:-}" = "1" ]; then
          printf 'SUBCOMMAND=%s\n' "$SUBCOMMAND"
          printf 'STDIO=%s\n' "$USE_STDIO"
          printf 'WORKSPACE=%s\n' "$PROJECT_DIR"
          printf 'IMAGE_OVERRIDE_REF=%s\n' "$IMAGE_OVERRIDE_REF"
          printf 'IMAGE_OVERRIDE_SOURCE=%s\n' "$IMAGE_OVERRIDE_SOURCE"
          printf 'IMAGE_OVERRIDE_DIGEST=%s\n' "$IMAGE_OVERRIDE_DIGEST"
          for pair in "''${SPAWN_ENV[@]}"; do printf 'ENV=%s\n' "$pair"; done
          for arg in "''${CONTAINER_CMD[@]}"; do printf 'CMD=%s\n' "$arg"; done
          for entry in "''${SPAWN_MOUNTS[@]}"; do printf 'MOUNT=-v %s\n' "$entry"; done
          exit 0
        fi

        ${cleanStaleStagingDirs}

        ${createStagingDir}

        ${expandPathFn}

        verbose "Project dir: $PROJECT_DIR"

        # Ensure the per-workspace wrapix-beads dolt container is running
        # before launching. Without it, bd inside the container has no
        # server to talk to and the socket at $PROJECT_DIR/.wrapix/dolt.sock
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
        # ~/.claude is container-local (tmpfs); entrypoint symlinks persistent items.
        # The dir is best-effort: when `wrapix spawn` is invoked from inside
        # another wrapix sandbox, $PROJECT_DIR is the host path and is not
        # visible to the caller's filesystem. Skip on permission errors — the
        # container's entrypoint creates the dir again when WRAPIX_AGENT=claude.
        #
        # mktemp the error-capture file so concurrent `wrapix spawn`
        # invocations don't race on a shared `/tmp/wrapix-mkdir-err`
        # path (wx-w4h5e).
        mkdir_err_file=$(mktemp)
        trap 'rm -f "$mkdir_err_file"' EXIT INT TERM
        if ! mkdir -p "$PROJECT_DIR/.claude" 2>"$mkdir_err_file"; then
            mkdir_err=$(cat "$mkdir_err_file")
            case "$mkdir_err" in
                *"Permission denied"*)
                    verbose "Skipping host-side .claude prep — $PROJECT_DIR not accessible from this context: $mkdir_err"
                    ;;
                *)
                    echo "wrapix: mkdir $PROJECT_DIR/.claude failed: $mkdir_err" >&2
                    exit 1
                    ;;
            esac
        fi
        rm -f "$mkdir_err_file"
        trap - EXIT INT TERM

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
            if [ "$mode" = "rw" ]; then
              # rw caches (sccache, cargo registry/git, uv) must persist
              # across container exits — bind-mount directly so writes land
              # on the host. STAGING_ROOT is rm -rf'd on exit (lib/util/shell.nix),
              # which would discard all cache writes.
              VOLUME_ARGS="$VOLUME_ARGS -v $src:$dest:$mode"
            else
              # ro mounts may dereference into the nix store; stage with
              # cp -rL so the container sees content, not dangling symlinks.
              staging="$STAGING_ROOT/dir$dir_idx"
              mkdir -p "$staging"
              cp -rL "$src/." "$staging/"
              dir_idx=$((dir_idx + 1))
              VOLUME_ARGS="$VOLUME_ARGS -v $staging:$dest:$mode"
            fi
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

        # Per-launch SpawnConfig.mounts are additive on top of profile.mounts
        # — podman applies them as plain `-v host:container[:ro]` binds. The
        # launcher does not stat host_path; podman fails loudly at runtime if
        # the source is missing.
        for entry in "''${SPAWN_MOUNTS[@]}"; do
          VOLUME_ARGS="$VOLUME_ARGS -v $entry"
        done

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

        # Mount deploy key and signing key (not under ~/.ssh/ — see lib/util/ssh.nix).
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
        if [ -n "$DEPLOY_KEY" ]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $DEPLOY_KEY:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME:ro"
          DEPLOY_KEY_ARGS="-e WRAPIX_DEPLOY_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
        fi
        if [ -n "$SIGNING_KEY" ]; then
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

        # Image install transport: skopeo copy oci-archive: → containers-storage:
        # (per specs/sandbox.md § Image install path). Body lives in
        # `lib/util/shell.nix` and is shared with the wrapix-spawn-load verifier.
        ${imageLoadStep}
        # Prune stale wrapix-* tags from every profile on every invocation,
        # not just after a fresh load — otherwise a cached current profile
        # lets stale hashes from other profiles accumulate indefinitely.
        ${pruneStaleImages { }}

        verbose "Starting container (cpus=$CPUS, memory=${toString memoryMb}m)..."

        # Detect krun availability for microVM boundary (see specs/security.md)
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

        # Per-subcommand wiring:
        #   run   — interactive: TTY allocated, host env passthrough.
        #   spawn — non-TTY: stdio piped, env strictly from SpawnConfig.
        TTY_ARGS=()
        ENV_ARGS=()
        if [ "$SUBCOMMAND" = "spawn" ]; then
          [ "$USE_STDIO" = "1" ] && TTY_ARGS=(-i)
          for pair in "''${SPAWN_ENV[@]}"; do
            ENV_ARGS+=(-e "$pair")
          done
          # Mark stream-json mode so the entrypoint dispatches into the
          # claude --print --input-format stream-json branch instead of the
          # interactive TTY fallback. Symmetric to the pi RPC branch which
          # is gated on WRAPIX_AGENT=pi.
          [ "$USE_STDIO" = "1" ] && ENV_ARGS+=(-e "WRAPIX_STDIO=1")
        else
          TTY_ARGS=(-i -t)
          ENV_ARGS+=(
            -e "CLAUDE_CODE_OAUTH_TOKEN=''${CLAUDE_CODE_OAUTH_TOKEN:-}"
            -e "WRAPIX_SESSION_ID=$WRAPIX_SESSION_ID"
            -e "WRAPIX_VERBOSE=''${WRAPIX_VERBOSE:-}"
          )
          [ -n "''${WRAPIX_GIT_SIGN:-}" ] && ENV_ARGS+=(-e "WRAPIX_GIT_SIGN=$WRAPIX_GIT_SIGN")
        fi
        # Always-on container env: built from launcher state, not host passthrough.
        ENV_ARGS+=(
          -e "BD_NO_DAEMON=1"
          -e "HOME=/home/wrapix"
          -e "GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME"
          -e "GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL"
          -e "GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME"
          -e "GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL"
          -e "WRAPIX_NETWORK=$WRAPIX_NETWORK"
          -e "WRAPIX_NETWORK_ALLOWLIST=${networkAllowlist}"
        )
        [ -n "$KRUN_CMD_ENV" ] && ENV_ARGS+=(-e "$KRUN_CMD_ENV")

        RUN_IMAGE="$IMAGE_REF"

        # shellcheck disable=SC2086 # Intentional word splitting for volume args
        exec podman run --rm "''${TTY_ARGS[@]}" \
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
          ${
            concatStringsSep " " (
              map (d: "--mount type=tmpfs,destination=${d},U=true") (profile.writableDirs or [ ])
            )
          } \
          $VOLUME_ARGS \
          $BEADS_ARGS \
          $DEPLOY_KEY_ARGS \
          $PODMAN_SOCKET_ARGS \
          $KRUN_ENV_ARGS \
          "''${ENV_ARGS[@]}" \
          -w /workspace \
          "$RUN_IMAGE" \
          "''${CONTAINER_CMD[@]}"
      '';
    };
}
