{ pkgs }:

let
  selected = pkgs.writeShellScriptBin "consumer-launcher" ''
    set -euo pipefail
    case "$*" in
      'run --help') printf '%s\n' 'Use generated project launch data';;
      'service capabilities') printf '%s\n' 'shared-services-v1';;
      'run') printf '%s\n' "selected:$WRIX_PROJECT_PROFILE_CONFIG";;
      'service start')
        printf '%s\n' start >> "$CALLS"
        [[ "''${FAIL_ENTRY:-0}" == 0 ]]
        ;;
      *) exit 2;;
    esac
  '';
  stale = pkgs.writeShellScriptBin "wrix" ''
    set -euo pipefail
    printf '%s\n' 'wrix requires --profile-config <Nix store ProfileConfig JSON>' >&2
    exit 2
  '';
  legacy = pkgs.writeShellScriptBin "legacy-wrix" ''
    set -euo pipefail
    case "$*" in
      'run --help') printf '%s\n' 'Use generated project launch data';;
      'service start') printf '%s\n' legacy-start >> "$CALLS";;
      *) exit 2;;
    esac
  '';
  devshell = import ../../lib/devshell {
    inherit pkgs;
    rustCli = { };
    beads = import ../../lib/beads { inherit pkgs; };
  };
  config = pkgs.writeText "fixture-profile.json" "{}";
  shell = devshell.mkDevShell {
    sandbox = {
      profile = {
        hostPackages = [ ];
        hostEnv = { };
      };
      image.profileConfig = config;
    };
    env.WRIX_BIN = "${selected}/bin/consumer-launcher";
    nixCache = false;
    prekHooks = false;
    shellHook = ''
      wrix service start || { return 1 2>/dev/null || exit 1; }
    '';
  };
  hook = pkgs.writeText "fixture-shell-hook.sh" shell.shellHook;
in
pkgs.runCommand "wrix-devshell-cli-selection" { nativeBuildInputs = [ pkgs.bash ]; } ''
  set -euo pipefail
  export HOME="$TMPDIR/home"
  mkdir -p "$HOME" workspace
  cd workspace
  export CALLS="$TMPDIR/calls"
  export WRIX_PROJECT_PROFILE_CONFIG=${config}
  export PATH="${stale}/bin:$PATH"

  # Failed entry leaves the caller's stale environment unchanged.
  if (
    export WRIX_BIN=${selected}/bin/consumer-launcher
    export FAIL_ENTRY=1
    source ${hook}
  ); then
    echo 'failed entry unexpectedly succeeded' >&2
    exit 1
  fi
  [[ "$(type -P wrix)" == ${stale}/bin/wrix ]]
  [[ "$(<"$CALLS")" == start ]]

  # Successful entry dispatches bare wrix through the explicit consumer wrapper.
  (
    export WRIX_BIN=${selected}/bin/consumer-launcher
    source ${hook}
    [[ "$WRIX_BIN" == ${selected}/bin/consumer-launcher ]]
    [[ "$(wrix run)" == selected:${config} ]]
  )

  # Stale host admission fails before the consumer's service-start effect.
  : > "$CALLS"
  if (
    unset WRIX_BIN
    source ${hook}
  ) 2> "$TMPDIR/stale.err"; then
    echo 'stale host was admitted' >&2
    exit 1
  fi
  [[ ! -s "$CALLS" ]]
  ${pkgs.gnugrep}/bin/grep -F 'nix profile add' "$TMPDIR/stale.err"
  ${pkgs.gnugrep}/bin/grep -F 'Home Manager, nix-darwin' "$TMPDIR/stale.err"
  # Service compatibility is required even without project launch data.
  (
    unset WRIX_PROJECT_PROFILE_CONFIG
    export WRIX_BIN=${selected}/bin/consumer-launcher
    source ${hook}
  )

  # Generated-project support alone must not admit the old two-container CLI.
  for launch_data in generated absent; do
    : > "$CALLS"
    if (
      [[ "$launch_data" != absent ]] || unset WRIX_PROJECT_PROFILE_CONFIG
      export WRIX_BIN=${legacy}/bin/legacy-wrix
      source ${hook}
    ) 2> "$TMPDIR/legacy.err"; then
      echo "legacy service CLI was admitted: $launch_data" >&2
      exit 1
    fi
    [[ ! -s "$CALLS" ]]
    ${pkgs.gnugrep}/bin/grep -F 'lacks the shared-service protocol' "$TMPDIR/legacy.err"
    ${pkgs.gnugrep}/bin/grep -F '${legacy}/bin/legacy-wrix' "$TMPDIR/legacy.err"
  done
  touch "$out"
''
