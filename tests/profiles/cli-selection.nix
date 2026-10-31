{ pkgs }:

let
  client =
    revision:
    pkgs.writeShellScriptBin "wrix" ''
      set -euo pipefail
      case "$*" in
        'service capabilities') printf '%s\n' shared-services-v1;;
        'run') printf '%s\n' '${revision}:'"$WRIX_PROJECT_PROFILE_CONFIG";;
        'service start') printf '%s\n' '${revision}' >> "$CALLS";;
        *) exit 64;;
      esac
    '';
  first = client "first-pin";
  second = client "second-pin";
  stale = pkgs.writeShellScriptBin "wrix" ''
    set -euo pipefail
    printf '%s\n' ambient >> "$CALLS"
    exit 125
  '';
  config = pkgs.writeText "fixture-profile.json" "{}";
  library =
    pinned:
    import ../../lib/devshell {
      inherit pkgs;
      rustCli.wrix = pinned;
      beads = import ../../lib/beads { inherit pkgs; };
    };
  shell =
    pinned:
    (library pinned).mkDevShell {
      sandbox = {
        profile = {
          hostPackages = [ ];
          hostEnv = { };
        };
        image.profileConfig = config;
      };
      nixCache = false;
      prekHooks = false;
      shellHook = ''
        [[ "$WRIX_BIN" == ${pinned}/bin/wrix ]]
      '';
    };
  firstHook = pkgs.writeText "first-hook" (shell first).shellHook;
  secondHook = pkgs.writeText "second-hook" (shell second).shellHook;
  conflicting =
    builtins.tryEval
      ((library first).mkDevShell {
        profile = {
          hostPackages = [ ];
          hostEnv = { };
        };
        nixCache = false;
        prekHooks = false;
        env.WRIX_BIN = "${stale}/bin/wrix";
      }).drvPath;
in
assert !conflicting.success;
pkgs.runCommand "wrix-devshell-cli-selection" { nativeBuildInputs = [ pkgs.bash ]; } ''
  set -euo pipefail
  export HOME="$TMPDIR/home"
  mkdir -p "$HOME" workspace
  cd workspace
  export CALLS="$TMPDIR/calls"
  export WRIX_PROJECT_PROFILE_CONFIG=${config}
  export PATH="${stale}/bin:$PATH"
  export WRIX_BIN=${stale}/bin/wrix

  # Both first and repeated activation select the locked client, never ambient Wrix.
  for activation in first cached; do
    source ${firstHook}
    [[ "$(type -P wrix)" == ${first}/bin/wrix ]]
    [[ "$(wrix run)" == first-pin:${config} ]]
  done
  # A different consumer pin switches both dispatch surfaces coherently.
  source ${secondHook}
  [[ "$WRIX_BIN" == ${second}/bin/wrix ]]
  [[ "$(type -P wrix)" == ${second}/bin/wrix ]]
  [[ "$(wrix run)" == second-pin:${config} ]]

  # Shell entry selects software; it neither starts nor updates services here.
  [[ ! -e "$CALLS" ]]
  wrix service start
  [[ "$(<"$CALLS")" == second-pin ]]
  touch "$out"
''
