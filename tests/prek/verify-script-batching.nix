{ pkgs }:
let
  alpha = pkgs.writeText "verify-alpha.sh" ''
    printf '%s\n' alpha >> "$executions"
    isolation=changed
    cd "$TMPDIR"
    trap 'touch "$TMPDIR/alpha-exit"' EXIT
  '';
  beta = pkgs.writeText "verify-beta.sh" ''printf '%s\n' beta >> "$executions"; return 23'';
  gamma = pkgs.writeText "verify-gamma.sh" ''false | true; printf '%s\n' false-pass >> "$executions"'';
  alphaDrv = builtins.unsafeDiscardStringContext alpha.drvPath;
  betaDrv = builtins.unsafeDiscardStringContext beta.drvPath;
  fakeNix = pkgs.writeShellScriptBin "nix" ''
    set -euo pipefail
    printf '%s\n' "$*" >> "$calls"
    if [[ "$1" == build && "$2" == --dry-run ]]; then
      if [[ -e "$failure/plan" ]]; then exit 19; fi
      if [[ -e "$failure/malformed" ]]; then printf '{}\n'; exit 0; fi
      printf '%s\n' '${
        builtins.toJSON [
          { drvPath = alphaDrv; }
          { drvPath = betaDrv; }
        ]
      }'
      exit 0
    fi
    [[ "$1" == build ]] || exit 64
    link=""
    previous=""
    for argument in "$@"; do
      if [[ "$previous" == --out-link ]]; then link="$argument"; fi
      previous="$argument"
    done
    selector="''${!#}"
    case "$selector" in
      ${alphaDrv}'^*'|*verifyScripts.*alpha*) output=${alpha} ;;
      ${betaDrv}'^*'|*verifyScripts.*beta*) output=${beta} ;;
      *verifyScripts.*gamma*) output=${gamma} ;;
      *) exit 64 ;;
    esac
    if [[ -n "$link" ]]; then
      if [[ -e "$failure/build" ]]; then exit 19; fi
      if [[ -e "$failure/identity" ]]; then output=${beta}; fi
      ln -sf "$output" "$link"
    else
      printf '%s\n' "$output"
    fi
  '';
  instantiate = pkgs.writeShellScriptBin "nix-instantiate" ''
    set -euo pipefail
    root=""
    previous=""
    for argument in "$@"; do
      if [[ "$previous" == --add-root ]]; then root="$argument"; fi
      previous="$argument"
    done
    [[ -n "$root" ]] || exit 64
    if [[ -e "$failure/root" ]]; then exit 19; fi
    ln -s ${alphaDrv} "$root-1"
    ln -s ${betaDrv} "$root-2"
    printf '%s\n' "$root-1" "$root-2"
  '';
in
pkgs.runCommand "verify-script-batching-conformance"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.jq
      pkgs.coreutils
      fakeNix
      instantiate
    ];
  }
  ''
    set -euo pipefail
    export calls="$TMPDIR/calls" executions="$TMPDIR/executions" failure="$TMPDIR/failure"
    mkdir "$failure"
    source ${../lib/verify-scripts.sh}
    trap verify_scripts_cleanup EXIT
    verify_scripts_prepare frozen-source unit alpha beta alpha
    test "$(grep -c '^build --dry-run' "$calls")" = 1
    test "$(awk '/--out-link/ { count++ } END { print count + 0 }' "$calls")" = 0
    script=$(verify_script_path frozen-source unit alpha)
    test "$script" = ${alpha}
    isolation=original
    before="$PWD"
    verify_script_run frozen-source unit alpha
    test "$isolation" = original
    test "$PWD" = "$before"
    test -f "$TMPDIR/alpha-exit"
    test -d "$verify_script_directory"
    script=$(verify_script_path frozen-source unit beta)
    if verify_script_run frozen-source unit beta; then exit 1; else test "$?" = 23; fi
    if verify_script_run frozen-source unit gamma; then exit 1; else test "$?" = 1; fi
    test "$(cat "$executions")" = $'alpha\nbeta'
    saved="$verify_script_directory"
    verify_scripts_cleanup
    test ! -e "$saved"
    for mode in plan malformed root; do
      (
        source ${../lib/verify-scripts.sh}
        touch "$failure/$mode"
        verify_scripts_prepare frozen-source unit alpha beta
        test "$(verify_script_path frozen-source unit alpha)" = ${alpha}
        rm "$failure/$mode"
        verify_scripts_cleanup
      )
    done
    for mode in build identity; do
      (
        source ${../lib/verify-scripts.sh}
        verify_scripts_prepare frozen-source unit alpha beta
        touch "$failure/$mode"
        test "$(verify_script_path frozen-source unit alpha)" = ${alpha}
        rm "$failure/$mode"
        verify_scripts_cleanup
      )
    done
    touch "$out"
  ''
