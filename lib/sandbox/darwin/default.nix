# Darwin sandbox using Apple container CLI (macOS 26+)
{
  pkgs,
  serviceCli,
}:

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
    rememberImageRef
    stageBeads
    ;

  knownHosts = import ../known-hosts.nix { inherit pkgs; };
  paths = import ../../util/path.nix { };
  shellLib = import ../../util/shell.nix { inherit pkgs; };
  sshConfig = import ../../util/ssh.nix;

  promptDir = writeTextDir "wrix-prompt" (readFile ../prompt.txt);
  serviceBin = "${serviceCli}/bin/wrix";

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
    writeShellScriptBin "wrix" ''
            set -euo pipefail

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
            if ! PROFILE_SCHEMA=$(${pkgs.jq}/bin/jq -er '.schema' "$PROFILE_CONFIG"); then
              echo "Error: invalid ProfileConfig JSON: $PROFILE_CONFIG" >&2
              exit 1
            fi
            if [[ "$PROFILE_SCHEMA" != "1" ]]; then
              echo "Error: unsupported ProfileConfig schema: $PROFILE_SCHEMA" >&2
              exit 1
            fi
            if ! PROFILE_AGENT=$(${pkgs.jq}/bin/jq -er '.agent.kind | select(. == "direct" or . == "claude" or . == "pi")' "$PROFILE_CONFIG"); then
              echo "Error: ProfileConfig agent.kind must be direct, claude, or pi" >&2
              exit 1
            fi
            if ! PROFILE_IMAGE_REF=$(${pkgs.jq}/bin/jq -er '.image.ref | strings | select(length > 0)' "$PROFILE_CONFIG"); then
              echo "Error: ProfileConfig image.ref must be a non-empty string" >&2
              exit 1
            fi
            if ! PROFILE_IMAGE_SOURCE=$(${pkgs.jq}/bin/jq -er '.image.source | strings | select(length > 0)' "$PROFILE_CONFIG"); then
              echo "Error: ProfileConfig image.source must be a non-empty string" >&2
              exit 1
            fi
            if ! PROFILE_IMAGE_SOURCE_KIND=$(${pkgs.jq}/bin/jq -er '.image.source_kind | select(. == "docker-archive")' "$PROFILE_CONFIG"); then
              echo "Error: ProfileConfig image.source_kind must be docker-archive on Darwin" >&2
              exit 1
            fi
            PROFILE_IMAGE_DIGEST=$(${pkgs.jq}/bin/jq -r '.image.digest // ""' "$PROFILE_CONFIG")
            PROFILE_NETWORK_ALLOWLIST=$(${pkgs.jq}/bin/jq -r '(.profile.network_allowlist // []) | join(",")' "$PROFILE_CONFIG")
            PROFILE_NIX_CACHE_ENABLE=$(${pkgs.jq}/bin/jq -r 'if ((.services.nix_cache.enable // true) == false or (.services.nix_cache.enable // true) == "false") then "0" else "1" end' "$PROFILE_CONFIG")
            PROFILE_CPUS=$(${pkgs.jq}/bin/jq -r '.resources.cpus // ""' "$PROFILE_CONFIG")
            PROFILE_MEMORY_MB=$(${pkgs.jq}/bin/jq -r '.resources.memory_mb // 4096' "$PROFILE_CONFIG")
            PROFILE_PIDS_LIMIT=$(${pkgs.jq}/bin/jq -r '.resources.pids_limit // 4096' "$PROFILE_CONFIG")
            WRIX_AGENT="$PROFILE_AGENT"
            WRIX_PROJECT_CACHE_URL=""
            WRIX_PROJECT_CACHE_HOST=""
            WRIX_PROJECT_CACHE_PORT=""
            WRIX_PROJECT_CACHE_NIX_CONFIG=""
            BEADS_DOLT_HOST=""
            BEADS_DOLT_PORT=""
            WRIX_WORKSPACE_DOLT=0

            wrix_sandbox_cache_host() {
              local host="$1"
              local resolved="''${WRIX_PROJECT_CACHE_SANDBOX_HOST:-$host}"
              if [[ ! "$resolved" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                echo "Error: project cache sandbox host must be a numeric IPv4 address: $resolved" >&2
                exit 1
              fi
              printf '%s\n' "$resolved"
            }

            wrix_configure_project_cache() {
              local endpoints cache_host cache_port state_root public_key_path public_key
              endpoints=$(cd "$PROJECT_DIR" && "${serviceBin}" service endpoints)
              if ! ${pkgs.jq}/bin/jq -e '.endpoints.cache_http != null' <<<"$endpoints" >/dev/null; then
                return 0
              fi
              cache_host=$(${pkgs.jq}/bin/jq -er '.endpoints.cache_http.host | strings | select(length > 0)' <<<"$endpoints")
              cache_port=$(${pkgs.jq}/bin/jq -er '.endpoints.cache_http.port | numbers' <<<"$endpoints")
              state_root=$(${pkgs.jq}/bin/jq -er '.state_root | strings | select(length > 0)' <<<"$endpoints")
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
              wrix_detect_workspace_dolt
              if [[ "$PROFILE_NIX_CACHE_ENABLE" = "1" ]]; then
                (cd "$PROJECT_DIR" && "${serviceBin}" service start >/dev/null)
                wrix_configure_project_cache
              elif [[ "$WRIX_WORKSPACE_DOLT" = "1" ]]; then
                (cd "$PROJECT_DIR" && "${serviceBin}" service start --no-cache >/dev/null)
              fi
              if [[ "$WRIX_WORKSPACE_DOLT" = "1" ]]; then
                BEADS_DOLT_PORT=$(cd "$PROJECT_DIR" && "${serviceBin}" service dolt port)
                BEADS_DOLT_HOST=$(cd "$PROJECT_DIR" && "${serviceBin}" service dolt host)
              fi
            }

            # Subcommand dispatch: `wrix run` (interactive, TTY) vs
            # `wrix spawn` (stdio, JSON spawn-config).
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
              if ! ${pkgs.jq}/bin/jq -e 'type == "object"' "$SPAWN_CONFIG" >/dev/null; then
                echo "Error: invalid SpawnConfig JSON: expected object" >&2
                exit 1
              fi
              PROFILE_OVERRIDE_FIELD=$(${pkgs.jq}/bin/jq -r 'first(["agent", "agent_kind", "wrix_agent", "WRIX_AGENT", "profile", "profile_name", "profile_config", "image_agent", "image-agent"][] as $field | select(has($field)) | $field) // ""' "$SPAWN_CONFIG")
              if [[ -n "$PROFILE_OVERRIDE_FIELD" ]]; then
                echo "Error: SpawnConfig cannot change the ProfileConfig agent/profile/image-agent field: $PROFILE_OVERRIDE_FIELD" >&2
                exit 1
              fi
              UNDOCUMENTED_OVERRIDE_FIELD=$(${pkgs.jq}/bin/jq -r 'first(["image_digest", "image_digest_path"][] as $field | select(has($field)) | $field) // ""' "$SPAWN_CONFIG")
              if [[ -n "$UNDOCUMENTED_OVERRIDE_FIELD" ]]; then
                echo "Error: SpawnConfig field $UNDOCUMENTED_OVERRIDE_FIELD is not a documented per-launch override; use a matching ProfileConfig image" >&2
                exit 1
              fi
              if ! ${pkgs.jq}/bin/jq -e '(
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
              PROJECT_DIR=$(${pkgs.jq}/bin/jq -r '.workspace' "$SPAWN_CONFIG")
              IMAGE_OVERRIDE_REF=$(${pkgs.jq}/bin/jq -r '.image_ref // ""' "$SPAWN_CONFIG")
              IMAGE_OVERRIDE_SOURCE=$(${pkgs.jq}/bin/jq -r '.image_source // ""' "$SPAWN_CONFIG")
              IMAGE_OVERRIDE_SOURCE_KIND=$(${pkgs.jq}/bin/jq -r '.image_source_kind // ""' "$SPAWN_CONFIG")
              if [[ -n "$IMAGE_OVERRIDE_SOURCE" && -z "$IMAGE_OVERRIDE_SOURCE_KIND" ]]; then
                echo "Error: SpawnConfig image_source requires image_source_kind" >&2
                exit 1
              fi
              if [[ -n "$IMAGE_OVERRIDE_SOURCE_KIND" && "$IMAGE_OVERRIDE_SOURCE_KIND" != "docker-archive" ]]; then
                echo "Error: SpawnConfig image_source_kind must be docker-archive on Darwin" >&2
                exit 1
              fi
              while IFS= read -r pair; do
                [[ -z "$pair" ]] && continue
                SPAWN_ENV+=("$pair")
              done < <(${pkgs.jq}/bin/jq -r '.env[] | "\(.[0])=\(.[1])"' "$SPAWN_CONFIG")
              while IFS= read -r arg; do
                CONTAINER_CMD+=("$arg")
              done < <(${pkgs.jq}/bin/jq -r '.agent_args[]' "$SPAWN_CONFIG")
              while IFS= read -r entry; do
                [[ -z "$entry" ]] && continue
                SPAWN_MOUNTS+=("$entry")
              done < <(${pkgs.jq}/bin/jq -r '(.mounts? // [])[] | [.host_path, .container_path, (.read_only|tostring)] | @tsv' "$SPAWN_CONFIG")
            else
              PROJECT_DIR="''${1:-$(pwd)}"
              shift || true
              # Remaining args override the container command (passed to entrypoint as $@)
              if [ $# -gt 0 ]; then
                CONTAINER_CMD=("$@")
              fi
            fi

            if [[ "''${WRIX_DRY_RUN:-}" = "1" && "''${WRIX_DRY_RUN_SERVICES:-0}" = "1" ]]; then
              wrix_ensure_workspace_services
            fi

            # WRIX_DRY_RUN=1: print resolved spawn state, run the mount
            # classifier with filesystem ops disabled, dump classified mount
            # intents, and exit before any container CLI invocation. Used by
            # tests to verify SpawnConfig parsing and mount classification
            # without a runtime.
            if [ "''${WRIX_DRY_RUN:-}" = "1" ]; then
              printf 'SUBCOMMAND=%s\n' "$SUBCOMMAND"
              printf 'STDIO=%s\n' "$USE_STDIO"
              printf 'PROFILE_CONFIG=%s\n' "$PROFILE_CONFIG"
              printf 'PROFILE_AGENT=%s\n' "$PROFILE_AGENT"
              printf 'WORKSPACE=%s\n' "$PROJECT_DIR"
              printf 'IMAGE_OVERRIDE_REF=%s\n' "$IMAGE_OVERRIDE_REF"
              printf 'IMAGE_OVERRIDE_SOURCE=%s\n' "$IMAGE_OVERRIDE_SOURCE"
              printf 'IMAGE_OVERRIDE_SOURCE_KIND=%s\n' "$IMAGE_OVERRIDE_SOURCE_KIND"
              if [[ -n "$WRIX_PROJECT_CACHE_URL" ]]; then
                printf 'PROJECT_CACHE_URL=%s\n' "$WRIX_PROJECT_CACHE_URL"
                printf 'ENV=WRIX_PROJECT_CACHE_HOST=%s\n' "$WRIX_PROJECT_CACHE_HOST"
                printf 'ENV=WRIX_PROJECT_CACHE_PORT=%s\n' "$WRIX_PROJECT_CACHE_PORT"
                printf 'ENV=NIX_CONFIG=%s\n' "$WRIX_PROJECT_CACHE_NIX_CONFIG"
              fi
              if [[ -n "$BEADS_DOLT_PORT" ]]; then
                printf 'ENV=BEADS_DOLT_SERVER_PORT=%s\n' "$BEADS_DOLT_PORT"
              fi
              if [[ -n "$BEADS_DOLT_HOST" ]]; then
                printf 'ENV=BEADS_DOLT_SERVER_HOST=%s\n' "$BEADS_DOLT_HOST"
              fi
              for pair in "''${SPAWN_ENV[@]}"; do printf 'ENV=%s\n' "$pair"; done
              for arg in "''${CONTAINER_CMD[@]}"; do printf 'CMD=%s\n' "$arg"; done
              for entry in "''${SPAWN_MOUNTS[@]}"; do printf 'MOUNT=%s\n' "$entry"; done
            fi

            if [ "''${WRIX_DRY_RUN:-}" != "1" ]; then
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

              wrix_ensure_workspace_services

              ${fixVmnetRoute}
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

            PROFILE_IMAGE="$IMAGE_REF"
            IMAGE_REPO="''${IMAGE_REF%:*}"
            if [ "''${WRIX_DRY_RUN:-}" != "1" ]; then
              if [ -z "$IMAGE_SOURCE" ]; then
                verbose "Using cached image $PROFILE_IMAGE"
              else
                # Content-digest preflight (specs/sandbox.md § Image install path):
                # short-circuit the load pipeline when an image with matching OCI
                # config digest is already in the platform store under any tag.
                # Apple's container CLI exposes no digest-as-ref lookup, so we
                # enumerate wrix-* refs and compare each ref's content digest.
                # On a hit, the requested ref is aliased to the matching content
                # and no tar bytes are streamed, no skopeo conversion, no
                # `container image load` is invoked. SpawnConfig source overrides
                # derive this digest from the selected docker-archive before load.
                _wrix_skip_load=0
                case "$IMAGE_SOURCE_KIND" in
                  docker-archive) ;;
                  *)
                    echo "Error: unsupported image source_kind on Darwin: $IMAGE_SOURCE_KIND" >&2
                    exit 1
                    ;;
                esac
                _wrix_desired_digest=""
                if [[ -n "''${IMAGE_DIGEST_PATH:-}" ]]; then
                  if [[ "$IMAGE_DIGEST_PATH" =~ ^sha256:[0-9a-f]{64}$ ]]; then
                    _wrix_desired_digest="$IMAGE_DIGEST_PATH"
                  elif [[ -s "$IMAGE_DIGEST_PATH" ]]; then
                    _wrix_digest_candidate=$(cat "$IMAGE_DIGEST_PATH")
                    if [[ "$_wrix_digest_candidate" =~ ^sha256:[0-9a-f]{64}$ ]]; then
                      _wrix_desired_digest="$_wrix_digest_candidate"
                    fi
                  fi
                fi
                if [[ -z "$_wrix_desired_digest" ]]; then
                  if ! _wrix_desired_digest=$(${pkgs.skopeo}/bin/skopeo inspect --raw "docker-archive:$IMAGE_SOURCE" | ${pkgs.jq}/bin/jq -er '.config.digest // empty | strings | select(test("^sha256:[0-9a-f]{64}$"))'); then
                    echo "Error: docker-archive image source is missing a sha256 digest: $IMAGE_SOURCE" >&2
                    exit 1
                  fi
                fi

                if [[ -n "$_wrix_desired_digest" ]]; then
                  _wrix_desired_short="''${_wrix_desired_digest#sha256:}"
                  _wrix_match_ref=""
                  while IFS= read -r _ref; do
                    [ -z "$_ref" ] && continue
                    _wrix_actual=$(container image inspect "$_ref" 2>/dev/null | ${pkgs.jq}/bin/jq -r '.[0].digest // .[0].id // empty')
                    _wrix_actual_short="''${_wrix_actual#sha256:}"
                    if [ -n "$_wrix_actual_short" ] && [ "$_wrix_actual_short" = "$_wrix_desired_short" ]; then
                      _wrix_match_ref="$_ref"
                      break
                    fi
                  done < <(container image list 2>/dev/null | tail -n +2 | awk '/^wrix-/ {print $1 ":" $2}')

                  if [ -n "$_wrix_match_ref" ]; then
                    # best-effort: requested ref may already alias matching content,
                    # in which case tag exits non-zero benignly; tar bytes still
                    # aren't streamed.
                    container image tag "$_wrix_match_ref" "$PROFILE_IMAGE" 2>/dev/null || true
                    _wrix_skip_load=1
                  fi
                elif container image inspect "$PROFILE_IMAGE" >/dev/null 2>&1; then
                  _wrix_skip_load=1
                fi

                if [ "$_wrix_skip_load" = "1" ]; then
                  verbose "Using cached image $PROFILE_IMAGE"
                else
                  verbose "Image hash changed or missing, reloading..."
                  echo "Loading profile image..."
                  case "$IMAGE_SOURCE_KIND" in
                    docker-archive)
                      # Drop the prior :latest alias so the new load can claim the same
                      # repo name; pruneStaleImages later cleans residual hash tags.
                      container image delete "$IMAGE_REPO:latest" 2>/dev/null || true
                      # Convert Docker-format tar to OCI-archive for Apple container CLI.
                      # --insecure-policy is safe: images are built locally from Nix
                      # derivations (trusted source with cryptographic hashes).
                      OCI_TAR="$WRIX_CACHE/profile-image-oci.tar"
                      mkdir -p "$WRIX_CACHE"
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
                      ;;
                  esac
                  verbose "Loaded image $PROFILE_IMAGE"
                fi
              fi
              ${rememberImageRef}
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
            GIT_AUTHOR_NAME="''${GIT_AUTHOR_NAME:-$(git config --global user.name 2>/dev/null || echo 'Wrix Sandbox')}"
            GIT_AUTHOR_EMAIL="''${GIT_AUTHOR_EMAIL:-$(git config --global user.email 2>/dev/null || echo 'sandbox@wrix.dev')}"
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
                if [ "''${WRIX_DRY_RUN:-}" != "1" ]; then
                  mkdir -p "$host_staging"
                  cp -rL "$src/." "$host_staging/"
                fi

                staging="/mnt/wrix/dir$dir_idx"
                dir_idx=$((dir_idx + 1))
                MOUNT_ARGS="$MOUNT_ARGS -v $host_staging:$staging"
                [ -n "$DIR_MOUNTS" ] && DIR_MOUNTS="$DIR_MOUNTS,"
                DIR_MOUNTS="$DIR_MOUNTS$staging:$dest"
              elif [ -S "$src" ]; then
                echo "wrix: Unix-socket mount source rejected: $src -> $dest" >&2
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
                  staging="/mnt/wrix/file$file_idx"
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

            if [ "''${WRIX_DRY_RUN:-}" = "1" ]; then
              [ -n "$DIR_MOUNTS" ] && printf 'DIR_MOUNTS=%s\n' "$DIR_MOUNTS"
              [ -n "$FILE_MOUNTS" ] && printf 'FILE_MOUNTS=%s\n' "$FILE_MOUNTS"
              printf 'MOUNT_ARGS=%s\n' "$MOUNT_ARGS"
              exit 0
            fi

            # Add SSH known_hosts and system prompt (directories from Nix store)
            # Note: prompt mounted to /etc/wrix-prompts (not /etc/wrix) to preserve
            # claude-config.json and claude-settings.json baked into /etc/wrix by image.nix
            MOUNT_ARGS="$MOUNT_ARGS -v ${knownHosts}:${sshConfig.knownHostsDirTarget}"
            MOUNT_ARGS="$MOUNT_ARGS -v ${promptDir}:/etc/wrix-prompts"

            # Notifications use TCP to gateway (port 5959) instead of mounted Unix socket
            # VirtioFS cannot pass Unix socket operations, so the container client
            # connects to the host daemon via the explicit TCP endpoint below.

            # Add deploy key and signing key (not under ~/.ssh/ — see lib/util/ssh.nix).
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
            if [[ -n "$DEPLOY_KEY" || -n "$SIGNING_KEY" ]]; then
              DEPLOY_STAGING="$STAGING_ROOT/deploy_keys"
              mkdir -p "$DEPLOY_STAGING"
              MOUNT_ARGS="$MOUNT_ARGS -v $DEPLOY_STAGING:/mnt/wrix/deploy_keys"
            fi
            if [[ -n "$DEPLOY_KEY" ]]; then
              cp "$DEPLOY_KEY" "$DEPLOY_STAGING/$DEPLOY_KEY_NAME"
              [[ -n "$FILE_MOUNTS" ]] && FILE_MOUNTS="$FILE_MOUNTS,"
              FILE_MOUNTS="$FILE_MOUNTS/mnt/wrix/deploy_keys/$DEPLOY_KEY_NAME:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
              DEPLOY_KEY_ARGS="-e WRIX_DEPLOY_KEY=${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME"
            fi
            if [[ -n "$SIGNING_KEY" ]]; then
              cp "$SIGNING_KEY" "$DEPLOY_STAGING/$DEPLOY_KEY_NAME-signing"
              [[ -n "$FILE_MOUNTS" ]] && FILE_MOUNTS="$FILE_MOUNTS,"
              FILE_MOUNTS="$FILE_MOUNTS/mnt/wrix/deploy_keys/$DEPLOY_KEY_NAME-signing:${sshConfig.containerKeyDir}/$DEPLOY_KEY_NAME-signing"
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

            # Pi credentials use a private one-file staging mount on Darwin.
            PI_AUTH_JSON_MOUNT=""
            PI_AUTH_STAGING_FILE=""
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
              PI_AUTH_NAME=$(basename "$PI_AUTH_FILE")
              PI_AUTH_STAGING_DIR="$STAGING_ROOT/pi-agent-auth"
              PI_AUTH_STAGING_FILE="$PI_AUTH_STAGING_DIR/$PI_AUTH_NAME"
              mkdir -p "$PI_AUTH_STAGING_DIR"
              cp "$PI_AUTH_FILE" "$PI_AUTH_STAGING_FILE"
              chmod 600 "$PI_AUTH_STAGING_FILE"
              MOUNT_ARGS="$MOUNT_ARGS -v $PI_AUTH_STAGING_DIR:/mnt/wrix/pi-agent-auth"
              PI_AUTH_JSON_MOUNT="/mnt/wrix/pi-agent-auth/$PI_AUTH_NAME"
            fi

            ${stageBeads}
            if [ -n "$BEADS_STAGING" ]; then
              MOUNT_ARGS="$MOUNT_ARGS -v $BEADS_STAGING:/workspace/.beads"
            fi

            # Session registration for focus-aware notifications (tmux only)
            WRIX_SESSION_ID=""
            WRIX_SESSION_FILE=""
            if [[ -n "''${TMUX:-}" ]]; then
              WRIX_SESSION_ID=$(tmux display-message -p '#S:#I.#P')
              WRIX_SESSION_DIR="''${XDG_DATA_HOME:-$HOME/.local/share}/wrix/sessions"
              mkdir -p "$WRIX_SESSION_DIR"

              # Capture terminal app name (no sudo required, may need Accessibility permission)
              TERMINAL_APP=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null || echo "")

              # Use safe filename (replace : and . with -)
              SAFE_SESSION_ID="''${WRIX_SESSION_ID//[:\.]/-}"
              WRIX_SESSION_FILE="$WRIX_SESSION_DIR/$SAFE_SESSION_ID.json"
              printf '{"session_id":"%s","terminal_app":"%s"}\n' "$WRIX_SESSION_ID" "$TERMINAL_APP" > "$WRIX_SESSION_FILE"
            fi

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

            # Build environment arguments (use array to handle spaces in values)
            # In spawn mode env passthrough comes strictly from the
            # SpawnConfig allowlist; interactive run keeps the historical
            # host-env passthrough.
            ENV_ARGS=()
            if [ "$SUBCOMMAND" = "spawn" ]; then
              for pair in "''${SPAWN_ENV[@]}"; do
                ENV_ARGS+=(-e "$pair")
              done
              [ "$USE_STDIO" = "1" ] && ENV_ARGS+=(-e "WRIX_STDIO=1")
            else
              ENV_ARGS+=(-e "WRIX_VERBOSE=''${WRIX_VERBOSE:-}")
              ENV_ARGS+=(-e "CLAUDE_CODE_OAUTH_TOKEN=''${CLAUDE_CODE_OAUTH_TOKEN:-}")
              [ -n "''${WRIX_GIT_SIGN:-}" ] && ENV_ARGS+=(-e "WRIX_GIT_SIGN=$WRIX_GIT_SIGN")
              ENV_ARGS+=(-e "WRIX_SESSION_ID=$WRIX_SESSION_ID")
            fi
            # Always-on container env: built from launcher state, not host passthrough.
            ENV_ARGS+=(-e "BD_NO_DAEMON=1")
            ENV_ARGS+=(-e "HOST_UID=$(id -u)")
            ENV_ARGS+=(-e "GIT_AUTHOR_NAME=$GIT_AUTHOR_NAME")
            ENV_ARGS+=(-e "GIT_AUTHOR_EMAIL=$GIT_AUTHOR_EMAIL")
            ENV_ARGS+=(-e "GIT_COMMITTER_NAME=$GIT_COMMITTER_NAME")
            ENV_ARGS+=(-e "GIT_COMMITTER_EMAIL=$GIT_COMMITTER_EMAIL")
            ENV_ARGS+=(-e "WRIX_AGENT=$WRIX_AGENT")
            [ -n "$DIR_MOUNTS" ] && ENV_ARGS+=(-e "WRIX_DIR_MOUNTS=$DIR_MOUNTS")
            [ -n "$FILE_MOUNTS" ] && ENV_ARGS+=(-e "WRIX_FILE_MOUNTS=$FILE_MOUNTS")
            ENV_ARGS+=(-e "WRIX_NOTIFY_TCP=192.168.64.1:5959")
            [ -n "$BEADS_DOLT_PORT" ] && ENV_ARGS+=(-e "BEADS_DOLT_SERVER_PORT=$BEADS_DOLT_PORT")
            [ -n "$BEADS_DOLT_HOST" ] && ENV_ARGS+=(-e "BEADS_DOLT_SERVER_HOST=$BEADS_DOLT_HOST")
            # Pass network mode and allowlist for WRIX_NETWORK=limit filtering
            ENV_ARGS+=(-e "WRIX_NETWORK=$WRIX_NETWORK")
            [ "$_vpn_conflict" = true ] && ENV_ARGS+=(-e "WRIX_WAIT_FOR_ROUTE=1")
            ENV_ARGS+=(-e "WRIX_NETWORK_ALLOWLIST=$PROFILE_NETWORK_ALLOWLIST")
            if [[ -n "$WRIX_PROJECT_CACHE_URL" ]]; then
              ENV_ARGS+=(-e "WRIX_PROJECT_CACHE_HOST=$WRIX_PROJECT_CACHE_HOST")
              ENV_ARGS+=(-e "WRIX_PROJECT_CACHE_PORT=$WRIX_PROJECT_CACHE_PORT")
              ENV_ARGS+=(-e "NIX_CONFIG=$WRIX_PROJECT_CACHE_NIX_CONFIG")
            fi
            [ -n "$PI_AUTH_JSON_MOUNT" ] && ENV_ARGS+=(-e "WRIX_PI_AUTH_JSON=$PI_AUTH_JSON_MOUNT")

            # Generate unique container name
            CONTAINER_NAME="wrix-$$"

            # Calculate CPUs (use ProfileConfig override or half of available, minimum 2)
            if [ -n "$PROFILE_CPUS" ]; then
              CPUS="$PROFILE_CPUS"
            else
              CPUS=$(($(sysctl -n hw.ncpu) / 2))
              [ "$CPUS" -lt 2 ] && CPUS=2
            fi

            # Ensure .claude directory exists on host for session persistence
            mkdir -p "$PROJECT_DIR/.claude"

            verbose "Starting container (cpus=$CPUS, memory=''${PROFILE_MEMORY_MB}M)..."

            # Run container
            # Note: -w / because WorkingDir=/workspace fails before mounts are ready
            # Note: ~/.claude is NOT mounted from host — the entrypoint selectively
            # symlinks persistent items from /workspace/.claude instead. This keeps
            # user-level settings.json separate from project-level settings.json,
            # avoiding Claude Code writing user-only properties (like
            # skipDangerousModePermissionPrompt) to the project settings path.
            TTY_ARGS=()
            if [ "$SUBCOMMAND" = "spawn" ]; then
              [ "$USE_STDIO" = "1" ] && TTY_ARGS=(-i)
            else
              [ -t 0 ] && TTY_ARGS=(-t -i)
            fi

            RUN_IMAGE="''${WRIX_IMAGE:-$PROFILE_IMAGE}"

            CONTAINER_EXIT=0
            container run \
              --name "$CONTAINER_NAME" \
              --rm \
              --cap-add CAP_NET_ADMIN \
              "''${TTY_ARGS[@]}" \
              -w / \
              -c "$CPUS" \
              -m "''${PROFILE_MEMORY_MB}M" \
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
            if [[ -n "''${PI_AUTH_STAGING_FILE:-}" ]]; then
              if [[ -f "$PI_AUTH_STAGING_FILE" ]]; then
                cp "$PI_AUTH_STAGING_FILE" "$PI_AUTH_FILE"
                chmod 600 "$PI_AUTH_FILE"
              else
                echo "wrix: Pi auth staging file disappeared: $PI_AUTH_STAGING_FILE" >&2
                CONTAINER_EXIT=1
              fi
            fi
            exit $CONTAINER_EXIT
    '';
}
