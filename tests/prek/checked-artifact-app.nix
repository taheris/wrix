{ pkgs }:
let
  checked = import ../lib/checked-artifact-app.nix { inherit pkgs; };
  raw =
    value:
    let
      payload = pkgs.writeText "checked-artifact-input" value;
    in
    pkgs.writeShellScriptBin "artifact-check" ''
      set -euo pipefail
      printf '%s' ${pkgs.lib.escapeShellArg value} | cmp ${payload} -
    '';
  package = raw "first";
  good = checked {
    inherit package;
    executable = "artifact-check";
  };
  changed = checked {
    package = raw "changed";
    executable = "artifact-check";
  };
  failed = checked {
    package = pkgs.writeShellScriptBin "artifact-check" ''
      set -euo pipefail
      exit 23
    '';
    executable = "artifact-check";
  };
  failureBody = pkgs.writeShellScript "failed-check-builder" failed.verificationResult.buildCommand;
  closure = pkgs.closureInfo { rootPaths = [ good ]; };
in
assert good.verificationResult.drvPath != changed.verificationResult.drvPath;
assert good.verificationResult.drvPath != failed.verificationResult.drvPath;
pkgs.runCommand "checked-artifact-app-conformance" { } ''
  set -euo pipefail
  ${good}/bin/artifact-check
  test -f ${good.verificationResult}
  IFS= read -r result < ${good}/nix-support/verification-result
  test "$result" = ${pkgs.lib.escapeShellArg (toString good.verificationResult)}
  if grep -Fx ${pkgs.lib.escapeShellArg (toString package)} ${closure}/store-paths; then
    echo 'successful check retained its image runner closure' >&2
    exit 1
  fi
  if out="$TMPDIR/failed-result" bash ${failureBody}; then
    echo 'failed artifact check produced a passing result' >&2
    exit 1
  fi
  test ! -e "$TMPDIR/failed-result"
  touch "$out"
''
