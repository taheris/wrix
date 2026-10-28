{ pkgs }:

pkgs.writeShellScriptBin "bd" ''
  set -euo pipefail

  if [[ ! -e /etc/wrix/image-agent ]]; then
    # Never reuse another repository's inherited SQL identity on the host.
    unset WRIX_BEADS_MANAGED WRIX_BEADS_SYNC_ENDPOINT WRIX_BEADS_WORKTREE
    unset BEADS_DOLT_SERVER_DATABASE BEADS_DOLT_SHARED_SERVER
    unset BEADS_DOLT_SERVER_USER BEADS_DOLT_PASSWORD BEADS_DOLT_SERVER_SOCKET
    unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
    root=$(git rev-parse --show-toplevel 2>/dev/null) || root=""
    if [[ -n "$root" && ( -f "$root/.beads/metadata.json" || -f "$root/.beads/dolt/WRIX-MANAGED" ) ]]; then
      client_env=$("''${WRIX_BIN:-wrix}" service dolt client-env) || exit 1
      eval "$client_env"
      unset client_env
    fi
  fi
  if [[ "''${WRIX_BEADS_MANAGED:-}" == 1 ]]; then
    # Recognize sync even after global output flags. Unknown argument layouts
    # fail closed in the scoped native client, never fall back to admin SQL.
    positional=()
    skip_value=0
    readonly=0
    literal=0
    for arg in "$@"; do
      if [[ "$literal" == 1 ]]; then
        positional+=("$arg")
        continue
      fi
      if [[ "$skip_value" == 1 ]]; then
        skip_value=0
        continue
      fi
      case "$arg" in
        --) literal=1 ;;
        -C|-C?*|--directory|--directory=*)
          echo 'Use cd before managed bd; -C/--directory scope is not supported.' >&2
          exit 1
          ;;
        --body|--description|--title|--notes|--design|--acceptance|--comment|-m)
          positional+=("$arg")
          skip_value=1
          ;;
        --database|--database=*|--db|--db=*|--global|--global=*)
          echo 'Managed Beads database scope is fixed by workspace admission.' >&2
          exit 1
          ;;
        --actor|--dolt-auto-commit|--mem-profile) skip_value=1 ;;
        --actor=*|--dolt-auto-commit=*|--mem-profile=*) ;;
        --readonly|--readonly=true) readonly=1 ;;
        --json|--quiet|-q|--verbose|-v|--no-color|--cpu-profile|--ignore-schema-skew|--sandbox|--readonly=false|--json=*|--quiet=*|--verbose=*|--no-color=*|--cpu-profile=*|--ignore-schema-skew=*|--sandbox=*) ;;
        *) positional+=("$arg") ;;
      esac
    done
    if [[ "''${positional[0]:-}" == dolt && ( "''${positional[1]:-}" == push || "''${positional[1]:-}" == pull ) ]]; then
      if [[ "$readonly" == 1 || "$skip_value" == 1 || "''${#positional[@]}" -ne 2 ]]; then
        echo 'Managed Beads sync accepts no remote/path overrides.' >&2
        exit 1
      fi
      exec "''${WRIX_BIN:-wrix}" service dolt "sync-''${positional[1]}"
    fi
  fi
  exec ${pkgs.beads}/bin/bd "$@"
''
