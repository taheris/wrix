# Linux sandbox implementation using a single container
{
  pkgs,
  serviceCli,
}:

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
    rememberImageRef
    pruneStaleImages
    stageBeads
    ;

  knownHosts = import ../known-hosts.nix { inherit pkgs; };
  paths = import ../../util/path.nix { };
  shellLib = import ../../util/shell.nix { inherit pkgs; };
  sshConfig = import ../../util/ssh.nix;

  prompt = writeText "wrix-prompt" (readFile ../prompt.txt);
  serviceBin = "${serviceCli}/bin/wrix";

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
      deployKey ? null,
      ...
    }:
    let
      deployKeyExpr = mkDeployKeyExpr deployKey;

    in
    writeShellApplication {
      name = "wrix";
      runtimeInputs = [
        crun-krun
        pkgs.jq
        pkgs.podman
        pkgs.skopeo
      ];
      text = ''
        # Verbose mode for debugging startup
        WRIX_VERBOSE="''${WRIX_VERBOSE:-}"
        verbose() { [ -n "$WRIX_VERBOSE" ] && echo "[wrix] $*" >&2 || true; }

        # Ensure USER is set (may be unset in some environments)
        USER="''${USER:-$(id -un)}"

        # XDG-compliant directories for staging and image cache
        XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}"
        WRIX_CACHE="$XDG_CACHE_HOME/wrix"

        PROFILE_CONFIG=""
        while [[ $# -gt 0 ]]; do
          case "$1" in
            --profile-config)
              [[ $# -lt 2 ]] && { echo "Error: --profile-config requires <file>" >&2; exit 2; }
              PROFILE_CONFIG="$2"; shift 2 ;;
            --profile-config=*) PROFILE_CONFIG="''${1#--profile-config=}"; shift ;;
            --) shift; break ;;
            *) break ;;
          esac
        done
        if [[ -z "$PROFILE_CONFIG" ]]; then
          echo "Error: wrix requires --profile-config <Nix store ProfileConfig JSON>" >&2
          exit 2
        fi
        if [[ ! -f "$PROFILE_CONFIG" ]]; then
          echo "Error: profile config not found: $PROFILE_CONFIG" >&2
          exit 1
        fi
        if ! PROFILE_SCHEMA=$(jq -er '.schema' "$PROFILE_CONFIG"); then
          echo "Error: invalid ProfileConfig JSON: $PROFILE_CONFIG" >&2
          exit 1
        fi
        if [[ "$PROFILE_SCHEMA" != "1" ]]; then
          echo "Error: unsupported ProfileConfig schema: $PROFILE_SCHEMA" >&2
          exit 1
        fi
        if ! PROFILE_AGENT=$(jq -er '.agent.kind | select(. == "direct" or . == "claude" or . == "pi")' "$PROFILE_CONFIG"); then
          echo "Error: ProfileConfig agent.kind must be direct, claude, or pi" >&2
          exit 1
        fi
        if ! PROFILE_IMAGE_REF=$(jq -er '.image.ref | strings | select(length > 0)' "$PROFILE_CONFIG"); then
          echo "Error: ProfileConfig image.ref must be a non-empty string" >&2
          exit 1
        fi
        if ! PROFILE_IMAGE_SOURCE=$(jq -er '.image.source | strings | select(length > 0)' "$PROFILE_CONFIG"); then
          echo "Error: ProfileConfig image.source must be a non-empty string" >&2
          exit 1
        fi
        if ! PROFILE_IMAGE_SOURCE_KIND=$(jq -er '.image.source_kind | select(. == "nix-descriptor")' "$PROFILE_CONFIG"); then
          echo "Error: ProfileConfig image.source_kind must be nix-descriptor on Linux" >&2
          exit 1
        fi
        PROFILE_IMAGE_DIGEST=$(jq -r '.image.digest // ""' "$PROFILE_CONFIG")
        PROFILE_NETWORK_ALLOWLIST=$(jq -r '(.profile.network_allowlist // []) | join(",")' "$PROFILE_CONFIG")
        PROFILE_NIX_CACHE_ENABLE=$(jq -r 'if ((.services.nix_cache.enable // true) == false or (.services.nix_cache.enable // true) == "false") then "0" else "1" end' "$PROFILE_CONFIG")
        PROFILE_CPUS=$(jq -r '.resources.cpus // ""' "$PROFILE_CONFIG")
        PROFILE_MEMORY_MB=$(jq -r '.resources.memory_mb // 4096' "$PROFILE_CONFIG")
        PROFILE_PIDS_LIMIT=$(jq -r '.resources.pids_limit // 4096' "$PROFILE_CONFIG")
        WRIX_AGENT="$PROFILE_AGENT"
        WRIX_PROJECT_CACHE_URL=""
        WRIX_PROJECT_CACHE_HOST=""
        WRIX_PROJECT_CACHE_PORT=""
        WRIX_PROJECT_CACHE_NIX_CONFIG=""
        BEADS_DOLT_CONTAINER_SOCKET=""
        BEADS_DOLT_SOCKET_MOUNT_SOURCE=""
        WRIX_WORKSPACE_DOLT=0
        WRIX_PASTA_HOST_LOOPBACK_IP="169.254.1.2"
        WRIX_PODMAN_NETWORK="pasta:--map-host-loopback,$WRIX_PASTA_HOST_LOOPBACK_IP,--map-guest-addr,none,-t,none,-u,none,-T,none,-U,none"

        wrix_sandbox_cache_host() {
          local host="$1"
          local resolved="''${WRIX_PROJECT_CACHE_SANDBOX_HOST:-}"
          if [[ -z "$resolved" ]]; then
            case "$host" in
              127.*) resolved="$WRIX_PASTA_HOST_LOOPBACK_IP" ;;
              *) resolved="$host" ;;
            esac
          fi
          if [[ ! "$resolved" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            echo "Error: project cache sandbox host must be a numeric IPv4 address: $resolved" >&2
            exit 1
          fi
          printf '%s\n' "$resolved"
        }

        wrix_configure_project_cache() {
          local endpoints cache_host cache_port state_root public_key_path public_key
          endpoints=$(cd "$PROJECT_DIR" && "${serviceBin}" service endpoints)
          if ! jq -e '.endpoints.cache_http != null' <<<"$endpoints" >/dev/null; then
            return 0
          fi
          cache_host=$(jq -er '.endpoints.cache_http.host | strings | select(length > 0)' <<<"$endpoints")
          cache_port=$(jq -er '.endpoints.cache_http.port | numbers' <<<"$endpoints")
          state_root=$(jq -er '.state_root | strings | select(length > 0)' <<<"$endpoints")
          public_key_path="$state_root/keys/cache.pub"
          if [[ ! -f "$public_key_path" ]]; then
            echo "Error: project cache public key not found: $public_key_path" >&2
            exit 1
          fi
          WRIX_PROJECT_CACHE_HOST=$(wrix_sandbox_cache_host "$cache_host")
          WRIX_PROJECT_CACHE_PORT="$cache_port"
          WRIX_PROJECT_CACHE_URL="http://$WRIX_PROJECT_CACHE_HOST:$WRIX_PROJECT_CACHE_PORT"
          public_key=$(tr -d '\n' <"$public_key_path")
          if ! [[ "$public_key" =~ ^[^:]+:[A-Za-z0-9+/]{43}=$ ]]; then
            echo "Error: project cache public key is invalid: $public_key_path" >&2
            exit 1
          fi
          WRIX_PROJECT_CACHE_NIX_CONFIG=$(printf 'extra-substituters = %s\nextra-trusted-public-keys = %s\nbuilders-use-substitutes = true' "$WRIX_PROJECT_CACHE_URL" "$public_key")
        }

        wrix_configure_dolt_socket() {
          local socket="$1"
          local project_real=""
          BEADS_DOLT_CONTAINER_SOCKET="/workspace/.wrix/dolt.sock"
          project_real=$(cd "$PROJECT_DIR" && pwd -P)
          if [[ "$socket" = "$project_real/.wrix/dolt.sock" ]]; then
            return 0
          fi
          BEADS_DOLT_SOCKET_MOUNT_SOURCE=$(dirname "$socket")
          BEADS_DOLT_CONTAINER_SOCKET="/run/wrix/dolt/dolt.sock"
        }

        wrix_detect_workspace_dolt() {
          local status_output
          if status_output=$(cd "$PROJECT_DIR" && "${serviceBin}" service dolt status 2>&1); then
            WRIX_WORKSPACE_DOLT=1
            return 0
          fi
          case "$status_output" in
            *"dolt: disabled"*) WRIX_WORKSPACE_DOLT=0 ;;
            *)
              printf '%s\n' "$status_output" >&2
              exit 1
              ;;
          esac
        }

        wrix_ensure_workspace_services() {
          local socket
          wrix_detect_workspace_dolt
          if [[ "$PROFILE_NIX_CACHE_ENABLE" = "1" ]]; then
            (cd "$PROJECT_DIR" && "${serviceBin}" service start >/dev/null)
            wrix_configure_project_cache
          elif [[ "$WRIX_WORKSPACE_DOLT" = "1" ]]; then
            (cd "$PROJECT_DIR" && "${serviceBin}" service start --no-cache >/dev/null)
          fi
          if [[ "$WRIX_WORKSPACE_DOLT" = "1" ]]; then
            socket=$(cd "$PROJECT_DIR" && "${serviceBin}" service dolt socket)
            wrix_configure_dolt_socket "$socket"
          fi
        }

        # Subcommand dispatch: `wrix run` (interactive, TTY) vs
        # `wrix spawn` (stdio, JSON spawn-config). Default with no
        # subcommand keeps legacy positional invocation `wrix [DIR] [CMD...]`.
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
        IMAGE_OVERRIDE_SOURCE_KIND=""
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
              *) echo "Error: unknown wrix spawn flag: $1" >&2; exit 2 ;;
            esac
          done
          if [ -z "$SPAWN_CONFIG" ]; then
            echo "Error: wrix spawn requires --spawn-config <file>" >&2
            exit 2
          fi
          if [ ! -f "$SPAWN_CONFIG" ]; then
            echo "Error: spawn-config file not found: $SPAWN_CONFIG" >&2
            exit 1
          fi
          if ! jq -e 'type == "object"' "$SPAWN_CONFIG" >/dev/null; then
            echo "Error: invalid SpawnConfig JSON: expected object" >&2
            exit 1
          fi
          PROFILE_OVERRIDE_FIELD=$(jq -r 'first(["agent", "agent_kind", "wrix_agent", "WRIX_AGENT", "profile", "profile_name", "profile_config", "image_agent", "image-agent"][] as $field | select(has($field)) | $field) // ""' "$SPAWN_CONFIG")
          if [[ -n "$PROFILE_OVERRIDE_FIELD" ]]; then
            echo "Error: SpawnConfig cannot change the ProfileConfig agent/profile/image-agent field: $PROFILE_OVERRIDE_FIELD" >&2
            exit 1
          fi
          UNDOCUMENTED_OVERRIDE_FIELD=$(jq -r 'first(["image_digest", "image_digest_path"][] as $field | select(has($field)) | $field) // ""' "$SPAWN_CONFIG")
          if [[ -n "$UNDOCUMENTED_OVERRIDE_FIELD" ]]; then
            echo "Error: SpawnConfig field $UNDOCUMENTED_OVERRIDE_FIELD is not a documented per-launch override; use a matching ProfileConfig image" >&2
            exit 1
          fi
          if ! jq -e '(
            (.workspace | type == "string" and length > 0) and
            (.image_ref == null or (.image_ref | type == "string")) and
            (.image_source == null or (.image_source | type == "string")) and
            (.image_source_kind == null or (.image_source_kind | type == "string")) and
            (.env | type == "array") and
            all(.env[]; (type == "array") and (length == 2) and ((.[0] | type == "string" and length > 0) and (.[1] | type == "string"))) and
            (.agent_args | type == "array") and
            all(.agent_args[]; type == "string") and
            (.mounts == null or ((.mounts | type == "array") and all(.mounts[]; (type == "object") and ((.host_path | type == "string" and length > 0) and (.container_path | type == "string" and length > 0) and (.read_only | type == "boolean")))))
          )' "$SPAWN_CONFIG" >/dev/null; then
            echo "Error: invalid SpawnConfig schema: expected workspace string, optional image_ref/image_source/image_source_kind strings, env [key,value] string pairs, agent_args strings, and mounts with host_path/container_path/read_only" >&2
            exit 1
          fi
          PROJECT_DIR=$(jq -r '.workspace' "$SPAWN_CONFIG")
          IMAGE_OVERRIDE_REF=$(jq -r '.image_ref // ""' "$SPAWN_CONFIG")
          IMAGE_OVERRIDE_SOURCE=$(jq -r '.image_source // ""' "$SPAWN_CONFIG")
          IMAGE_OVERRIDE_SOURCE_KIND=$(jq -r '.image_source_kind // ""' "$SPAWN_CONFIG")
          if [[ -n "$IMAGE_OVERRIDE_SOURCE" && -z "$IMAGE_OVERRIDE_SOURCE_KIND" ]]; then
            echo "Error: SpawnConfig image_source requires image_source_kind" >&2
            exit 1
          fi
          if [[ -n "$IMAGE_OVERRIDE_SOURCE_KIND" && "$IMAGE_OVERRIDE_SOURCE_KIND" != "nix-descriptor" ]]; then
            echo "Error: SpawnConfig image_source_kind must be nix-descriptor on Linux" >&2
            exit 1
          fi
          while IFS= read -r pair; do
            [[ -z "$pair" ]] && continue
            SPAWN_ENV+=("$pair")
          done < <(jq -r '.env[] | "\(.[0])=\(.[1])"' "$SPAWN_CONFIG")
          while IFS= read -r arg; do
            CONTAINER_CMD+=("$arg")
          done < <(jq -r '.agent_args[]' "$SPAWN_CONFIG")
          while IFS= read -r entry; do
            [[ -z "$entry" ]] && continue
            SPAWN_MOUNTS+=("$entry")
          done < <(jq -r '(.mounts? // [])[] | "\(.host_path):\(.container_path)" + (if .read_only == true then ":ro" else "" end)' "$SPAWN_CONFIG")
        else
          PROJECT_DIR="''${1:-$(pwd)}"
          shift || true
          # Remaining args override the container command (passed to entrypoint as $@)
          if [ $# -gt 0 ]; then
            CONTAINER_CMD=("$@")
          fi
        fi

        if [[ "''${WRIX_DRY_RUN:-}" != "1" || "''${WRIX_DRY_RUN_SERVICES:-0}" = "1" ]]; then
          wrix_ensure_workspace_services
        fi

        # WRIX_DRY_RUN=1: print resolved spawn state and exit without
        # touching the filesystem or invoking podman. Used by tests to
        # verify SpawnConfig parsing and per-bead profile selection
        # without a container runtime.
        if [ "''${WRIX_DRY_RUN:-}" = "1" ]; then
          printf 'SUBCOMMAND=%s\n' "$SUBCOMMAND"
          printf 'STDIO=%s\n' "$USE_STDIO"
          printf 'PROFILE_CONFIG=%s\n' "$PROFILE_CONFIG"
          printf 'PROFILE_AGENT=%s\n' "$PROFILE_AGENT"
          printf 'WORKSPACE=%s\n' "$PROJECT_DIR"
          printf 'IMAGE_OVERRIDE_REF=%s\n' "$IMAGE_OVERRIDE_REF"
          printf 'IMAGE_OVERRIDE_SOURCE=%s\n' "$IMAGE_OVERRIDE_SOURCE"
          printf 'IMAGE_OVERRIDE_SOURCE_KIND=%s\n' "$IMAGE_OVERRIDE_SOURCE_KIND"
          printf 'PODMAN_NETWORK=%s\n' "$WRIX_PODMAN_NETWORK"
          if [[ -n "$WRIX_PROJECT_CACHE_URL" ]]; then
            printf 'PROJECT_CACHE_URL=%s\n' "$WRIX_PROJECT_CACHE_URL"
            printf 'ENV=WRIX_PROJECT_CACHE_HOST=%s\n' "$WRIX_PROJECT_CACHE_HOST"
            printf 'ENV=WRIX_PROJECT_CACHE_PORT=%s\n' "$WRIX_PROJECT_CACHE_PORT"
            printf 'ENV=NIX_CONFIG=%s\n' "$WRIX_PROJECT_CACHE_NIX_CONFIG"
          fi
          if [[ -n "$BEADS_DOLT_CONTAINER_SOCKET" ]]; then
            printf 'ENV=BEADS_DOLT_SERVER_SOCKET=%s\n' "$BEADS_DOLT_CONTAINER_SOCKET"
          fi
          if [[ -n "$BEADS_DOLT_SOCKET_MOUNT_SOURCE" ]]; then
            printf 'MOUNT=-v %s:/run/wrix/dolt:rw\n' "$BEADS_DOLT_SOCKET_MOUNT_SOURCE"
          fi
          for pair in "''${SPAWN_ENV[@]}"; do printf 'ENV=%s\n' "$pair"; done
          for arg in "''${CONTAINER_CMD[@]}"; do printf 'CMD=%s\n' "$arg"; done
          for entry in "''${SPAWN_MOUNTS[@]}"; do printf 'MOUNT=-v %s\n' "$entry"; done
          exit 0
        fi

        ${cleanStaleStagingDirs}

        ${createStagingDir}

        ${expandPathFn}

        verbose "Project dir: $PROJECT_DIR"

        # Read git author from host config (overrideable via env vars)
        GIT_AUTHOR_NAME="''${GIT_AUTHOR_NAME:-$(git config --global user.name 2>/dev/null || echo 'Wrix Sandbox')}"
        GIT_AUTHOR_EMAIL="''${GIT_AUTHOR_EMAIL:-$(git config --global user.email 2>/dev/null || echo 'sandbox@wrix.dev')}"
        GIT_COMMITTER_NAME="''${GIT_COMMITTER_NAME:-$GIT_AUTHOR_NAME}"
        GIT_COMMITTER_EMAIL="''${GIT_COMMITTER_EMAIL:-$GIT_AUTHOR_EMAIL}"

        # Build volume args
        VOLUME_ARGS="-v $PROJECT_DIR:/workspace:rw"
        if [[ -n "$BEADS_DOLT_SOCKET_MOUNT_SOURCE" ]]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $BEADS_DOLT_SOCKET_MOUNT_SOURCE:/run/wrix/dolt:rw"
        fi

        # Ensure project .claude dir exists for session persistence (/resume, /rename)
        # ~/.claude is container-local (tmpfs); entrypoint symlinks persistent items.
        # The dir is best-effort: when `wrix spawn` is invoked from inside
        # another wrix sandbox, $PROJECT_DIR is the host path and is not
        # visible to the caller's filesystem. Skip on permission errors — the
        # container's entrypoint creates the dir again when WRIX_AGENT=claude.
        #
        # mktemp the error-capture file so concurrent `wrix spawn`
        # invocations don't race on a shared `/tmp/wrix-mkdir-err`
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
                    echo "wrix: mkdir $PROJECT_DIR/.claude failed: $mkdir_err" >&2
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
        VOLUME_ARGS="$VOLUME_ARGS -v ${prompt}:/etc/wrix-prompt:ro"

        # Mount notification socket directory if daemon is running
        # We mount the directory (not the socket file) so daemon restarts work
        # without needing to restart the container
        NOTIFY_SOCKET_DIR="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix"
        if [ -S "$NOTIFY_SOCKET_DIR/notify.sock" ]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $NOTIFY_SOCKET_DIR:/run/wrix"
        else
          echo "Note: Notification socket not found at $NOTIFY_SOCKET_DIR/notify.sock" >&2
          echo "      Run 'nix run .#wrix-notifyd' on host for desktop notifications" >&2
        fi

        # Mount host podman socket for sibling container access (opt-in)
        PODMAN_SOCKET_ARGS=""
        if [ -n "''${WRIX_PODMAN_SOCKET:-}" ]; then
          PODMAN_SOCK="''${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/podman/podman.sock"
          if [ -S "$PODMAN_SOCK" ]; then
            PODMAN_SOCKET_ARGS="-v $PODMAN_SOCK:/run/podman/podman.sock -e CONTAINER_HOST=unix:///run/podman/podman.sock"
            # Tell nested podman commands where the host sees /workspace.
            # $PROJECT_DIR is the host path (the launcher runs on the host).
            PODMAN_SOCKET_ARGS="$PODMAN_SOCKET_ARGS -e GC_HOST_WORKSPACE=$PROJECT_DIR"
          else
            echo "Error: WRIX_PODMAN_SOCKET set but socket not found at $PODMAN_SOCK" >&2
            exit 1
          fi
        fi

        # Mount deploy key and signing key (not under ~/.ssh/ — see lib/util/ssh.nix).
        # Host-source resolution precedence per specs/security.md:
        #   1. $WRIX_{DEPLOY,SIGNING}_KEY pointing at an existing file.
        #   2. $HOME/.ssh/deploy_keys/<name>{,-signing} fallback.
        # Set-but-missing env is fail-loud (parent-process mistake).
        DEPLOY_KEY_NAME=${deployKeyExpr}
        DEPLOY_KEY=""
        if [[ -n "''${WRIX_DEPLOY_KEY:-}" ]]; then
          if [[ ! -f "$WRIX_DEPLOY_KEY" ]]; then
            echo "wrix: WRIX_DEPLOY_KEY=$WRIX_DEPLOY_KEY: file does not exist" >&2
            exit 1
          fi
          DEPLOY_KEY="$WRIX_DEPLOY_KEY"
        elif [[ -f "$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME" ]]; then
          DEPLOY_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME"
        fi
        SIGNING_KEY=""
        if [[ -n "''${WRIX_SIGNING_KEY:-}" ]]; then
          if [[ ! -f "$WRIX_SIGNING_KEY" ]]; then
            echo "wrix: WRIX_SIGNING_KEY=$WRIX_SIGNING_KEY: file does not exist" >&2
            exit 1
          fi
          SIGNING_KEY="$WRIX_SIGNING_KEY"
        elif [[ -f "$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing" ]]; then
          SIGNING_KEY="$HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing"
        fi
        DEPLOY_KEY_ARGS=""
        if [[ -n "$DEPLOY_KEY" ]]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $DEPLOY_KEY:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME:ro"
          DEPLOY_KEY_ARGS="-e WRIX_DEPLOY_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
        fi
        if [[ -n "$SIGNING_KEY" ]]; then
          VOLUME_ARGS="$VOLUME_ARGS -v $SIGNING_KEY:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing:ro"
          DEPLOY_KEY_ARGS="$DEPLOY_KEY_ARGS -e WRIX_SIGNING_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
        fi

        # spawn (loop agent) must sign and push: fail loud on a keyless boot, not at land-the-plane (specs/security.md § Deploy & Signing Keys).
        if [[ "$SUBCOMMAND" = "spawn" ]]; then
          if [[ -z "$DEPLOY_KEY" ]]; then
            echo "wrix spawn: no deploy key resolved — set WRIX_DEPLOY_KEY to an existing file, or place one at $HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME" >&2
            exit 1
          fi
          if [[ "''${WRIX_GIT_SIGN:-1}" != "0" && -z "$SIGNING_KEY" ]]; then
            echo "wrix spawn: no signing key resolved — set WRIX_SIGNING_KEY to an existing file, place one at $HOME/.ssh/deploy_keys/$DEPLOY_KEY_NAME-signing, or set WRIX_GIT_SIGN=0 to disable commit signing" >&2
            exit 1
          fi
        fi

        # Pi subscription credentials are file-backed. Mount only auth.json
        # when the selected image is Pi; settings are non-secret image defaults.
        PI_AUTH_JSON_MOUNT=""
        if [ "$WRIX_AGENT" = "pi" ]; then
          PI_AUTH_FILE="''${WRIX_PI_AUTH_FILE:-$HOME/.pi/agent/auth.json}"
          if [ -n "''${WRIX_PI_AUTH_FILE:-}" ]; then
            if [ ! -f "$PI_AUTH_FILE" ]; then
              echo "wrix: WRIX_PI_AUTH_FILE=$PI_AUTH_FILE: file does not exist" >&2
              exit 1
            fi
          elif [ "$SUBCOMMAND" = "spawn" ] && [ ! -f "$PI_AUTH_FILE" ]; then
            echo "wrix spawn: Pi auth file not found at $PI_AUTH_FILE — run 'pi' and /login on the host, or set WRIX_PI_AUTH_FILE to an existing auth.json" >&2
            exit 1
          elif [ "$SUBCOMMAND" = "run" ]; then
            mkdir -p "$(dirname "$PI_AUTH_FILE")"
            if [ ! -e "$PI_AUTH_FILE" ]; then
              printf '{}\n' > "$PI_AUTH_FILE"
            fi
            chmod 600 "$PI_AUTH_FILE"
          fi
          VOLUME_ARGS="$VOLUME_ARGS -v $PI_AUTH_FILE:/mnt/wrix/file/pi-auth.json:rw"
          PI_AUTH_JSON_MOUNT="/mnt/wrix/file/pi-auth.json"
        fi

        ${stageBeads}
        BEADS_ARGS=""
        if [ -n "$BEADS_STAGING" ]; then
          BEADS_ARGS="-v $BEADS_STAGING:/workspace/.beads"
          if [ -n "''${WRIX_PODMAN_SOCKET:-}" ]; then
            PODMAN_SOCKET_ARGS="$PODMAN_SOCKET_ARGS -e GC_HOST_BEADS=$BEADS_STAGING"
          fi
        fi

        # Session registration for focus-aware notifications (tmux only)
        WRIX_SESSION_ID=""
        WRIX_SESSION_FILE=""
        if [[ -n "''${TMUX:-}" ]]; then
          WRIX_SESSION_ID=$(tmux display-message -p '#S:#I.#P')
          WRIX_SESSION_DIR="''${XDG_RUNTIME_DIR:-$HOME/.local/share}/wrix/sessions"
          mkdir -p "$WRIX_SESSION_DIR"

          # Capture window ID for focus detection (niri-specific)
          WINDOW_ID=""
          if command -v niri >/dev/null 2>&1; then
            WINDOW_ID=$(niri msg -j focused-window 2>/dev/null | jq -r '.id // ""') || WINDOW_ID=""
          fi

          # Use safe filename (replace : and . with -)
          SAFE_SESSION_ID="''${WRIX_SESSION_ID//[:\.]/-}"
          WRIX_SESSION_FILE="$WRIX_SESSION_DIR/$SAFE_SESSION_ID.json"
          printf '{"session_id":"%s","window_id":"%s"}\n' "$WRIX_SESSION_ID" "$WINDOW_ID" > "$WRIX_SESSION_FILE"
        fi

        # Cleanup function for session file
        # shellcheck disable=SC2329 # Invoked via trap
        cleanup_session() {
          if [[ -n "$WRIX_SESSION_FILE" && -f "$WRIX_SESSION_FILE" ]]; then
            rm -f "$WRIX_SESSION_FILE"
          fi
        }
        trap cleanup_session EXIT

        # Validate WRIX_NETWORK mode (default: open)
        WRIX_NETWORK="''${WRIX_NETWORK:-open}"
        case "$WRIX_NETWORK" in
          open|limit) ;;
          *)
            echo "Error: WRIX_NETWORK must be 'open' or 'limit' (got: $WRIX_NETWORK)" >&2
            exit 1
            ;;
        esac

        # Baseline network filtering is always installed before the agent starts.
        NETWORK_CAP_ARGS="--cap-add=NET_ADMIN"

        # Calculate CPUs (use ProfileConfig override or half of available, minimum 2)
        if [ -n "$PROFILE_CPUS" ]; then
          CPUS="$PROFILE_CPUS"
        else
          CPUS=$(($(nproc) / 2))
          [ "$CPUS" -lt 2 ] && CPUS=2
        fi

        # Image defaults come from immutable ProfileConfig. SpawnConfig may
        # override image transport fields for orchestrators, but not the agent.
        IMAGE_REF=""
        IMAGE_SOURCE=""
        IMAGE_SOURCE_KIND=""
        IMAGE_DIGEST_PATH=""
        if [[ "$SUBCOMMAND" = "run" ]]; then
          IMAGE_REF="$PROFILE_IMAGE_REF"
          IMAGE_SOURCE="$PROFILE_IMAGE_SOURCE"
          IMAGE_SOURCE_KIND="$PROFILE_IMAGE_SOURCE_KIND"
          IMAGE_DIGEST_PATH="$PROFILE_IMAGE_DIGEST"
        else
          IMAGE_REF="''${IMAGE_OVERRIDE_REF:-$PROFILE_IMAGE_REF}"
          IMAGE_SOURCE="''${IMAGE_OVERRIDE_SOURCE:-$PROFILE_IMAGE_SOURCE}"
          IMAGE_SOURCE_KIND="''${IMAGE_OVERRIDE_SOURCE_KIND:-$PROFILE_IMAGE_SOURCE_KIND}"
          if [[ ( -z "$IMAGE_OVERRIDE_REF" || "$IMAGE_OVERRIDE_REF" == "$PROFILE_IMAGE_REF" ) && ( -z "$IMAGE_OVERRIDE_SOURCE" || "$IMAGE_OVERRIDE_SOURCE" == "$PROFILE_IMAGE_SOURCE" ) && ( -z "$IMAGE_OVERRIDE_SOURCE_KIND" || "$IMAGE_OVERRIDE_SOURCE_KIND" == "$PROFILE_IMAGE_SOURCE_KIND" ) ]]; then
            IMAGE_DIGEST_PATH="$PROFILE_IMAGE_DIGEST"
          else
            IMAGE_DIGEST_PATH=""
          fi
        fi
        verbose "Resolved ProfileConfig (agent=$WRIX_AGENT, profile_config=$PROFILE_CONFIG, image=$IMAGE_REF)"

        # Image install transport: descriptor-backed skopeo oci: → containers-storage:
        # (per specs/sandbox.md § Image install path). Body lives in
        # `lib/util/shell.nix` and is shared with the wrix-spawn-load verifier.
        ${imageLoadStep}
        ${rememberImageRef}
        # Prune stale wrix-* tags from every profile on every invocation,
        # not just after a fresh load — otherwise a cached current profile
        # lets stale hashes from other profiles accumulate indefinitely.
        ${pruneStaleImages { }}

        verbose "Starting container (cpus=$CPUS, memory=''${PROFILE_MEMORY_MB}m)..."

        # Detect krun availability for microVM boundary (see specs/security.md)
        # Default: container boundary (krun microVM currently disabled)
        # WRIX_MICROVM=1: explicit opt-in to microVM boundary
        RUNTIME_ARGS=""
        if [ "''${WRIX_MICROVM:-}" = "1" ]; then
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
          KRUN_ENV_ARGS="-e WRIX_TERM_ROWS=$TERM_ROWS -e WRIX_TERM_COLS=$TERM_COLS"

          # Serialize container command for krun-init.sh (preserves quoting)
          # Kept separate from KRUN_ENV_ARGS to avoid word-splitting the value
          if [ ''${#CONTAINER_CMD[@]} -gt 0 ]; then
            KRUN_CMD_ENV="WRIX_KRUN_CMD=$(printf '%q ' "''${CONTAINER_CMD[@]}")"
          fi

          # krun-relay execs /krun-init.sh which handles args via WRIX_KRUN_CMD
          CONTAINER_CMD=()
        fi

        # Boundary-dependent UID handling. The image bakes /nix/store as
        # root:root, so store-mutating nix ops (GC/replace/delete -> deletePath
        # -> fchmodat2) need the runtime user to own the store.
        #   microVM (krun)   — keep --userns=keep-id; krun maps the host user to
        #                       root inside the VM and krun-init.sh LD_PRELOADs
        #                       libfakeuid (krun-relay's PTY tolerates the uid
        #                       spoof; the default boundary's does not).
        #   default container — drop keep-id so rootless container-root maps to
        #                       the host user that owns the baked store and lands
        #                       /workspace files as the host UID. claude refuses
        #                       --dangerously-skip-permissions as root, so set
        #                       IS_SANDBOX=1 (claude's escape hatch) instead of
        #                       libfakeuid: that getuid->1000 spoof blanks claude's
        #                       TUI when really root here (wx-nsage). Works on ANY
        #                       host uid (it maps to container-0).
        if [ -n "$RUNTIME_ARGS" ]; then
          USERNS_ARGS="--userns=keep-id"
        else
          USERNS_ARGS=""
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
          # is gated on WRIX_AGENT=pi.
          [ "$USE_STDIO" = "1" ] && ENV_ARGS+=(-e "WRIX_STDIO=1")
        else
          TTY_ARGS=(-i -t)
          ENV_ARGS+=(
            -e "CLAUDE_CODE_OAUTH_TOKEN=''${CLAUDE_CODE_OAUTH_TOKEN:-}"
            -e "WRIX_SESSION_ID=$WRIX_SESSION_ID"
            -e "WRIX_VERBOSE=''${WRIX_VERBOSE:-}"
          )
          [ -n "''${WRIX_GIT_SIGN:-}" ] && ENV_ARGS+=(-e "WRIX_GIT_SIGN=$WRIX_GIT_SIGN")
        fi
        # Always-on container env: built from launcher state, not host passthrough.
        ENV_ARGS+=(
          -e "BD_NO_DAEMON=1"
          -e "HOME=/home/wrix"
          -e "GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME"
          -e "GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL"
          -e "GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME"
          -e "GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL"
          -e "WRIX_AGENT=$WRIX_AGENT"
          -e "WRIX_NETWORK=$WRIX_NETWORK"
          -e "WRIX_NETWORK_ALLOWLIST=$PROFILE_NETWORK_ALLOWLIST"
        )
        if [[ -n "$WRIX_PROJECT_CACHE_URL" ]]; then
          ENV_ARGS+=(-e "WRIX_PROJECT_CACHE_HOST=$WRIX_PROJECT_CACHE_HOST")
          ENV_ARGS+=(-e "WRIX_PROJECT_CACHE_PORT=$WRIX_PROJECT_CACHE_PORT")
          ENV_ARGS+=(-e "NIX_CONFIG=$WRIX_PROJECT_CACHE_NIX_CONFIG")
        fi
        [ -n "$PI_AUTH_JSON_MOUNT" ] && ENV_ARGS+=(-e "WRIX_PI_AUTH_JSON=$PI_AUTH_JSON_MOUNT")
        [ -n "$BEADS_DOLT_CONTAINER_SOCKET" ] && ENV_ARGS+=(-e "BEADS_DOLT_SERVER_SOCKET=$BEADS_DOLT_CONTAINER_SOCKET")
        [ -n "$KRUN_CMD_ENV" ] && ENV_ARGS+=(-e "$KRUN_CMD_ENV")
        # default boundary: the process is the store-owning rootless container-0
        # (see USERNS_ARGS above). Tell claude it is sandboxed so it permits
        # --dangerously-skip-permissions as root, rather than spoofing the uid
        # with libfakeuid — that spoof blanks claude's TUI here (wx-nsage). krun
        # sets IS_SANDBOX=1 from inside krun-init.sh instead.
        [ -z "$RUNTIME_ARGS" ] && ENV_ARGS+=(-e "IS_SANDBOX=1")

        RUN_IMAGE="$IMAGE_REF"

        # shellcheck disable=SC2086 # Intentional word splitting for volume args
        exec podman run --rm "''${TTY_ARGS[@]}" \
          $RUNTIME_ARGS \
          $KRUN_ENTRYPOINT_ARGS \
          $NETWORK_CAP_ARGS \
          --cpus="$CPUS" \
          --memory="''${PROFILE_MEMORY_MB}m" \
          --pids-limit="$PROFILE_PIDS_LIMIT" \
          --network="$WRIX_PODMAN_NETWORK" \
          $USERNS_ARGS \
          --passwd-entry "wrix:*:$(id -u):$(id -g)::/home/wrix:/bin/bash" \
          --mount type=tmpfs,destination=/home/wrix,U=true \
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
