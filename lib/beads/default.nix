{
  pkgs,
  wrix ? null,
}:

let
  inherit (pkgs.stdenv) isDarwin;

  wrixBin = if wrix == null then "wrix" else "${wrix}/bin/wrix";
  jqBin = "${pkgs.jq}/bin/jq";

  fail = ''
    _wrix_beads_fail() {
      echo "$1" >&2
      return 1 2>/dev/null || exit 1
    }
  '';

  runtimeCheck =
    if isDarwin then
      ''
        _wrix_runtime="''${WRIX_CONTAINER_RUNTIME:-container}"
        if ! command -v "$_wrix_runtime" >/dev/null 2>&1; then
          if [[ "$_wrix_runtime" == "container" ]] && command -v podman >/dev/null 2>&1; then
            export WRIX_CONTAINER_RUNTIME=podman
            _wrix_runtime=podman
          else
            _wrix_beads_fail "beads: .beads/dolt exists but no service container runtime is available — install Apple container or podman, or set WRIX_CONTAINER_RUNTIME"
          fi
        fi
        unset _wrix_runtime
      ''
    else
      ''
        _wrix_runtime="''${WRIX_CONTAINER_RUNTIME:-podman}"
        if ! command -v "$_wrix_runtime" >/dev/null 2>&1; then
          _wrix_beads_fail "beads: .beads/dolt exists but service runtime '$_wrix_runtime' is not on PATH — cannot start wrix service dolt"
        fi
        unset _wrix_runtime
      '';

  startDirect = ''
    if ! "$_wrix_service_bin" service start --no-cache; then
      _wrix_beads_fail "beads: wrix service start --no-cache failed — Dolt will not be available"
    fi
  '';

  startService =
    if isDarwin then
      startDirect
    else
      ''
        if command -v systemd-run >/dev/null 2>&1 \
           && command -v systemctl >/dev/null 2>&1 \
           && systemctl --user is-active dbus.service >/dev/null 2>&1; then
          if ! systemd-run --user --scope --quiet --collect \
               -- "$_wrix_service_bin" service start --no-cache; then
            _wrix_beads_fail "beads: wrix service start --no-cache failed — Dolt will not be available"
          fi
        else
          ${startDirect}
        fi
      '';

  waitAndExport = ''
    if [[ -d "$PWD/.beads/dolt" ]]; then
      ${fail}
      _wrix_service_bin="''${WRIX_BIN:-${wrixBin}}"
      if ! _wrix_endpoints=$("$_wrix_service_bin" service endpoints --no-cache); then
        _wrix_beads_fail "beads: wrix service endpoints failed — cannot export Dolt endpoint"
      fi
      _wrix_transport=$(printf '%s\n' "$_wrix_endpoints" | ${jqBin} -r '.endpoints.dolt.transport // empty')
      if [[ -z "$_wrix_transport" ]]; then
        _wrix_beads_fail "beads: wrix service did not publish a Dolt endpoint — refusing embedded Dolt fallback"
      fi
      case "$_wrix_transport" in
        unix)
          _wrix_socket=$(printf '%s\n' "$_wrix_endpoints" | ${jqBin} -r '.endpoints.dolt.socket // empty')
          if [[ -z "$_wrix_socket" ]]; then
            _wrix_beads_fail "beads: wrix service Dolt endpoint is missing its socket path"
          fi
          _wrix_waited=0
          while [[ ! -S "$_wrix_socket" && "$_wrix_waited" -lt 30 ]]; do
            sleep 0.2
            _wrix_waited=$((_wrix_waited + 1))
          done
          if [[ ! -S "$_wrix_socket" ]]; then
            _wrix_beads_fail "beads: Dolt socket did not appear at $_wrix_socket — refusing embedded Dolt fallback"
          fi
          export BEADS_DOLT_SERVER_SOCKET="$_wrix_socket"
          unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
          ;;
        tcp)
          _wrix_host=$(printf '%s\n' "$_wrix_endpoints" | ${jqBin} -r '.endpoints.dolt.host // empty')
          _wrix_port=$(printf '%s\n' "$_wrix_endpoints" | ${jqBin} -r '.endpoints.dolt.port // empty')
          if [[ -z "$_wrix_host" || -z "$_wrix_port" ]]; then
            _wrix_beads_fail "beads: wrix service Dolt endpoint is missing TCP host or port"
          fi
          _wrix_waited=0
          while ! bash -c "echo >/dev/tcp/$_wrix_host/$_wrix_port" 2>/dev/null && [[ "$_wrix_waited" -lt 30 ]]; do
            sleep 0.2
            _wrix_waited=$((_wrix_waited + 1))
          done
          if ! bash -c "echo >/dev/tcp/$_wrix_host/$_wrix_port" 2>/dev/null; then
            _wrix_beads_fail "beads: Dolt TCP endpoint $_wrix_host:$_wrix_port is not reachable — refusing embedded Dolt fallback"
          fi
          export BEADS_DOLT_SERVER_HOST="$_wrix_host"
          export BEADS_DOLT_SERVER_PORT="$_wrix_port"
          unset BEADS_DOLT_SERVER_SOCKET
          ;;
        *)
          _wrix_beads_fail "beads: unsupported wrix service Dolt transport '$_wrix_transport'"
          ;;
      esac
      export BEADS_DOLT_AUTO_START=0
      unset _wrix_service_bin _wrix_endpoints _wrix_transport _wrix_socket _wrix_host _wrix_port _wrix_waited
    fi
  '';

  shellHook = ''
    if [[ -d "$PWD/.beads/dolt" ]]; then
      ${fail}
      _wrix_service_bin="''${WRIX_BIN:-${wrixBin}}"
      ${runtimeCheck}
      ${startService}
      unset _wrix_service_bin
      ${waitAndExport}
    fi
  '';

in
{
  inherit shellHook waitAndExport;
}
