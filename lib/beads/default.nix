{
  pkgs,
  linuxPkgs ? pkgs,
}:

let
  inherit (builtins) readFile;
  inherit (pkgs) lib;
  inherit (pkgs.stdenv) isDarwin;
  inherit (lib) optionals;

  mkImage =
    if isDarwin then
      linuxPkgs.dockerTools.buildLayeredImage
    else
      linuxPkgs.dockerTools.streamLayeredImage;

  imageTagLib = import ../util/image-tag.nix { };
  imageTag = imageTagLib.mkImageTag doltImageDrv;
  imageName = if isDarwin then "wrapix-beads:${imageTag}" else "localhost/wrapix-beads:${imageTag}";
  loadImageCmd = if isDarwin then "cat ${doltImageDrv}" else "${doltImageDrv}";
  shellLib = import ../util/shell.nix { };
  containerRuntime = builtins.readFile ../util/container.sh;

  # Minimal dolt-only container image used to serve a workspace's .beads/dolt.
  doltImageDrv = mkImage {
    name = "wrapix-beads";
    tag = "latest";
    maxLayers = 10;
    contents = with linuxPkgs; [
      bashInteractive
      coreutils
      dockerTools.caCertificates
      dolt
    ];
    config = {
      Env = [ "PATH=/bin:/usr/bin" ];
    };
  };

  # Runtime tools beads-dolt shells out to. Baked into the script's PATH so
  # consumers (devShell, tests, live gc daemon) don't have to know.
  # No dolt here — all dolt calls happen inside the container so host dolt
  # never touches the data dir (avoids host /tmp leaking into noms state).
  cliRuntimePath = lib.makeBinPath (
    with pkgs;
    [
      bashInteractive
      coreutils
    ]
    ++ optionals isDarwin [
      jq
      skopeo
    ]
  );

  # Host CLI: manages the per-workspace dolt container.
  #
  # Container name and listening port are derived from sha256(workspace path)
  # so two checkouts of the same repo at different paths get separate
  # containers and ports, and repeated invocations from the same workspace
  # reuse the same container.
  beadsDolt = pkgs.writeShellScriptBin "beads-dolt" ''
    set -euo pipefail

    # Self-contained runtime: dolt (for user grant), bash for /dev/tcp,
    # coreutils for basic tools. Prepended so we don't rely on host PATH.
    export PATH="${cliRuntimePath}:''${PATH:-}"

    # Container runtime abstraction (cr_* functions)
    ${containerRuntime}

    if [[ "$CR" == "container" ]]; then
      IMAGE="wrapix-beads:${imageTag}"
    else
      IMAGE="localhost/wrapix-beads:${imageTag}"
    fi

    ${
      if isDarwin then
        ''
          XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}"
          WRAPIX_CACHE="$XDG_CACHE_HOME/wrapix"
        ''
      else
        ""
    }

    _hash() {
      printf '%s' "''${1:-$PWD}" | sha256sum | cut -c1-8
    }

    _name() {
      echo "$(basename "''${1:-$PWD}")-beads"
    }

    # Port in [13306, 13805]. 500 slots is plenty for any realistic dev host.
    _port() {
      local h
      h=$(_hash "''${1:-$PWD}")
      printf '%d\n' $((13306 + 16#$h % 500))
    }

    _socket_path() {
      echo "''${1:-$PWD}/.wrapix/dolt.sock"
    }

    _load_image() {
      if cr_image_exists "$IMAGE"; then
        return 0
      fi
      echo "Loading beads image..." >&2
      case "$CR" in
        container)
          ${
            if isDarwin then
              ''
                local oci_tar="$WRAPIX_CACHE/beads-image-oci.tar"
                mkdir -p "$WRAPIX_CACHE"
                ${pkgs.skopeo}/bin/skopeo --insecure-policy copy --quiet \
                  "docker-archive:${doltImageDrv}" "oci-archive:$oci_tar"
                local load_out loaded_ref
                load_out=$($CR image load --input "$oci_tar" 2>&1)
                loaded_ref=$(echo "$load_out" | grep -oE 'untagged@sha256:[a-f0-9]+' | head -1)
                if [ -n "$loaded_ref" ]; then
                  cr_image_tag "$loaded_ref" "$IMAGE"
                  cr_image_tag "$loaded_ref" "wrapix-beads:latest"
                fi
                rm -f "$oci_tar"
              ''
            else
              ''
                echo "beads-dolt: CR=container on non-Darwin build — this should not happen" >&2
                return 1
              ''
          }
          ;;
        *)
          ${loadImageCmd} | podman load -q >/dev/null
          cr_image_tag "localhost/wrapix-beads:latest" "$IMAGE" 2>/dev/null || true
          ;;
      esac
      ${
        if isDarwin then
          ''
            if [[ "$CR" == "container" ]]; then
              ${shellLib.pruneStaleImages { runtime = "container"; }}
            else
              ${shellLib.pruneStaleImages { }}
            fi
          ''
        else
          shellLib.pruneStaleImages { }
      }
    }

    # Translate container-local path to host-side path for nested podman.
    # No-op when GC_HOST_WORKSPACE is unset (normal case) or on Darwin
    # (Apple Containers don't have nested-container host-socket patterns).
    _host_path() {
      local p="$1"
      [[ -n "''${GC_HOST_WORKSPACE:-}" ]] || { echo "$p"; return; }
      local gw="''${GC_WORKSPACE:-/workspace}"
      if [[ -n "''${GC_HOST_BEADS:-}" && "$p" == "''${gw}/.beads"* ]]; then
        echo "''${GC_HOST_BEADS}''${p#''${gw}/.beads}"
        return
      fi
      if [[ "$p" == "''${gw}"* ]]; then
        echo "''${GC_HOST_WORKSPACE}''${p#''${gw}}"
        return
      fi
      echo "$p"
    }

    cmd_name() { _name "''${1:-$PWD}"; }
    cmd_port() { _port "''${1:-$PWD}"; }
    cmd_socket() { _socket_path "''${1:-$PWD}"; }

    cmd_host() {
      local ws="''${1:-$PWD}"
      case "$CR" in
        container) echo "192.168.64.1" ;;
        *) _name "$ws" ;;
      esac
    }

    cmd_status() {
      local ws="''${1:-$PWD}"
      local name port
      name=$(_name "$ws")
      port=$(_port "$ws")
      echo "workspace: $ws"
      echo "container: $name"
      echo "port:      $port"
      echo "state:     $(cr_status "$name")"
    }

    _ensure_network() {
      case "$CR" in
        container) return 0 ;;
        *)
          if podman network exists wrapix-dolt; then
            return 0
          fi
          if podman network create wrapix-dolt >/dev/null 2>&1; then
            return 0
          fi
          podman network exists wrapix-dolt
          ;;
      esac
    }

    # Detect and evict a non-container process squatting on our port.
    # Linux-only: uses ss and /proc which don't exist on Darwin.
    _evict_port_squatter() {
      local port="$1"
      command -v ss >/dev/null 2>&1 || return 0

      local info
      info=$(ss -tlnp "sport = :$port" 2>/dev/null) || true

      local rest="''${info#*pid=}"
      [ "$rest" = "$info" ] && return 0
      local pid="''${rest%%[!0-9]*}"
      [ -z "$pid" ] && return 0

      local exe
      exe=$(readlink "/proc/$pid/exe" 2>/dev/null) || true
      case "$exe" in
        */dolt) ;;
        *) return 0 ;;
      esac

      echo "beads-dolt: port $port squatted by stale host dolt (pid=$pid)" >&2
      echo "beads-dolt: killing to reclaim port for container" >&2
      kill "$pid" 2>/dev/null || true

      local w=20
      while [ $w -gt 0 ]; do
        ss -tlnp "sport = :$port" 2>/dev/null | grep -q "pid=" || return 0
        sleep 0.2
        w=$((w - 1))
      done
      echo "beads-dolt: warning: port $port still occupied after eviction" >&2
    }

    _clean_data_dir() {
      local data_dir="$1"
      find "$data_dir" -name LOCK -delete
      find "$data_dir" -type d \( -name temptf -o -name tmp \) \
        -exec sh -c 'rm -rf "$1"/* "$1"/.[!.]* 2>/dev/null || true' _ {} \;
      rm -f "$data_dir/.doltcfg/privileges.db"
    }

    _dolt_reachable() {
      local name="$1" port="$2" ws="$3"
      case "$CR" in
        container)
          cr_exec "$name" bash -c "echo > /dev/tcp/127.0.0.1/$port" 2>/dev/null || return 1
          ;;
        *)
          if [[ -n "''${CONTAINER_HOST:-}" ]]; then
            cr_exec "$name" bash -c "echo > /dev/tcp/127.0.0.1/$port" 2>/dev/null || return 1
          else
            bash -c "echo > /dev/tcp/127.0.0.1/$port" 2>/dev/null || return 1
          fi
          test -S "$(_socket_path "$ws")"
          ;;
      esac
    }

    _wait_for_dolt() {
      local name="$1" port="$2" ws="$3"
      local retries=50
      while [ $retries -gt 0 ]; do
        if _dolt_reachable "$name" "$port" "$ws"; then
          return 0
        fi
        sleep 0.2
        retries=$((retries - 1))
      done
      return 1
    }

    cmd_start() {
      local ws="''${1:-$PWD}"
      local name port data_dir
      name=$(_name "$ws")
      port=$(_port "$ws")
      data_dir="$ws/.beads/dolt"

      if [ ! -d "$data_dir" ]; then
        echo "beads-dolt: no .beads/dolt directory at $ws — nothing to serve" >&2
        return 2
      fi

      _evict_port_squatter "$port"

      _load_image

      if cr_exists "$name"; then
        local _running_id _expected_id
        if _running_id=$(cr_container_image "$name") \
           && _expected_id=$(cr_image_id "$IMAGE") \
           && [ "$_running_id" != "$_expected_id" ]; then
          echo "beads-dolt: $name pinned to stale image — recreating with $IMAGE" >&2
          cr_rm "$name"
        elif cr_is_running "$name" \
             && _dolt_reachable "$name" "$port" "$ws"; then
          return 0
        else
          cr_rm "$name"
        fi
      fi

      _clean_data_dir "$data_dir"
      rm -f "$(_socket_path "$ws")"

      _ensure_network

      local workspace_mount=()
      if [ "$ws" != "/workspace" ]; then
        workspace_mount=(-v "$(_host_path "$ws"):/workspace:rw")
      fi

      case "$CR" in
        container)
          $CR run -d \
            --name "$name" \
            --network default \
            -e HOME=/tmp/dolthome \
            -e DOLT_FORCE_LOCAL_TEMP_FILES=1 \
            -e DOLT_ROOT_HOST="%" \
            --publish "$port:$port" \
            -v "$data_dir:/data" \
            -v "$ws:$ws" \
            "''${workspace_mount[@]}" \
            -- \
            "$IMAGE" \
            bash -c '
              set -e
              mkdir -p /tmp/dolthome /tmp
              mkdir -p "$(dirname "$2")"
              exec dolt sql-server --data-dir /data -H 0.0.0.0 -P "$1" --socket "$2"
            ' -- "$port" "/workspace/.wrapix/dolt.sock" \
            >/dev/null
          ;;
        *)
          # Subshell closes inherited fds so conmon doesn't hold direnv's
          # pipes open (which blocks direnv from completing).
          (
            for _fd in /proc/self/fd/*; do
              _fd=''${_fd##*/}
              [[ "$_fd" =~ ^[0-9]+$ ]] && [ "$_fd" -gt 2 ] && eval "exec $_fd>&-" 2>/dev/null || true
            done
            podman run -d \
              --name "$name" \
              --restart=unless-stopped \
              --entrypoint "" \
              --label "wrapix.workspace=$ws" \
              --network wrapix-dolt \
              --userns=keep-id \
              -e HOME=/tmp/dolthome \
              -e DOLT_FORCE_LOCAL_TEMP_FILES=1 \
              -e DOLT_ROOT_HOST="%" \
              --tmpfs /tmp:rw,mode=1777 \
              -p "127.0.0.1:$port:$port" \
              -v "$(_host_path "$data_dir"):/data:rw" \
              -v "$(_host_path "$ws"):$ws:rw" \
              "''${workspace_mount[@]}" \
              "$IMAGE" \
              bash -c '
                set -e
                mkdir -p /tmp/dolthome
                mkdir -p "$(dirname "$2")"
                exec dolt sql-server --data-dir /data -H 0.0.0.0 -P "$1" --socket "$2"
              ' -- "$port" "/workspace/.wrapix/dolt.sock" \
              >/dev/null
          )
          ;;
      esac

      if ! _wait_for_dolt "$name" "$port" "$ws"; then
        echo "beads-dolt: container did not become ready" >&2
        cr_logs_tail "$name" 10 >&2
        return 1
      fi
    }

    cmd_stop() {
      local ws="''${1:-$PWD}"
      local name
      name=$(_name "$ws")
      if cr_exists "$name"; then
        cr_rm "$name"
      fi
    }

    cmd_attach() {
      local network="''${1:?beads-dolt attach requires a network name}"
      local ws="''${2:-$PWD}"
      local name
      name=$(_name "$ws")

      if ! cr_exists "$name"; then
        echo "beads-dolt attach: container $name does not exist — run 'beads-dolt start' first" >&2
        return 1
      fi

      cr_network_has "$name" "$network" && return 0
      cr_network_connect "$network" "$name"
    }

    case "''${1:-}" in
      start)  shift; cmd_start "$@" ;;
      stop)   shift; cmd_stop "$@" ;;
      status) shift; cmd_status "$@" ;;
      port)   shift; cmd_port "$@" ;;
      name)   shift; cmd_name "$@" ;;
      socket) shift; cmd_socket "$@" ;;
      host)   shift; cmd_host "$@" ;;
      attach) shift; cmd_attach "$@" ;;
      *)
        echo "Usage: beads-dolt {start|stop|status|port|name|socket|host|attach <network>} [workspace]" >&2
        exit 2
        ;;
    esac
  '';

  beadsPush = pkgs.writeShellScriptBin "beads-push" (readFile ../../scripts/beads-push);

  # Wait for the dolt server and export connection env vars.
  # On Darwin, Unix sockets created inside the podman VM are not reachable
  # from the host, so we use TCP host/port. On Linux the socket works.
  waitAndExport =
    if isDarwin then
      ''
        _beads_port=$(${beadsDolt}/bin/beads-dolt port "$PWD")
        _beads_waited=0
        while ! bash -c "echo >/dev/tcp/127.0.0.1/$_beads_port" 2>/dev/null && [ "$_beads_waited" -lt 30 ]; do
          sleep 0.2
          _beads_waited=$((_beads_waited + 1))
        done
        if ! bash -c "echo >/dev/tcp/127.0.0.1/$_beads_port" 2>/dev/null; then
          echo "beads: dolt TCP port $_beads_port not reachable — refusing to fall back to embedded mode" >&2
          return 1 2>/dev/null || exit 1
        fi
        export BEADS_DOLT_SERVER_HOST=127.0.0.1
        export BEADS_DOLT_SERVER_PORT="$_beads_port"
        unset BEADS_DOLT_SERVER_SOCKET
        export BEADS_DOLT_AUTO_START=0
        unset _beads_port _beads_waited
      ''
    else
      ''
        _beads_sock=$(${beadsDolt}/bin/beads-dolt socket "$PWD")
        _beads_waited=0
        while [ ! -S "$_beads_sock" ] && [ "$_beads_waited" -lt 30 ]; do
          sleep 0.2
          _beads_waited=$((_beads_waited + 1))
        done
        if [ ! -S "$_beads_sock" ]; then
          echo "beads: dolt socket did not appear at $_beads_sock — refusing to fall back to embedded mode" >&2
          return 1 2>/dev/null || exit 1
        fi
        export BEADS_DOLT_SERVER_SOCKET="$_beads_sock"
        export BEADS_DOLT_AUTO_START=0
        unset _beads_sock _beads_waited
      '';

  # Shell hook fragment: ensures per-workspace dolt is running and exports
  # connection info that bd connects through. Suppresses bd's embedded
  # autostart so failure to reach the server fails loudly instead of
  # silently forking a second dolt. No-op if the current directory isn't
  # a beads workspace.
  shellHook =
    if isDarwin then
      ''
        if [ -d "$PWD/.beads/dolt" ]; then
          if ! command -v container >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
            echo "beads: .beads/dolt exists but no container runtime is available — cannot start dolt server" >&2
            return 1 2>/dev/null || exit 1
          fi
          ${beadsDolt}/bin/beads-dolt start "$PWD"
          ${waitAndExport}
        fi
      ''
    else
      ''
        if [ -d "$PWD/.beads/dolt" ]; then
          if ! command -v podman >/dev/null 2>&1; then
            echo "beads: .beads/dolt exists but podman is not on PATH — cannot start dolt server" >&2
            return 1 2>/dev/null || exit 1
          fi
          ${beadsDolt}/bin/beads-dolt start "$PWD"
          ${waitAndExport}
        fi
      '';

in
{
  inherit imageName shellHook waitAndExport;

  # dolt/push are exposed here for the flake overlay (see wrapixBeadsPkgs in
  # flake.nix). Consumers should reach them via pkgs.beads-dolt / pkgs.beads-push.
  # `cli` is a convenience bundle of both scripts — used by mkRalph so a single
  # package entry puts both binaries on the host devShell PATH.
  image = doltImageDrv;
  dolt = beadsDolt;
  push = beadsPush;
  cli = pkgs.symlinkJoin {
    name = "beads-cli";
    paths = [
      beadsDolt
      beadsPush
    ];
  };
}
