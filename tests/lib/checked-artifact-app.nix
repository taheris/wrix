# Only hermetic artifact checks use this constructor. Host/runtime/store-query
# checks retain their live runner; capability admission still precedes builds.
{ pkgs }:
{ package, executable }:
let
  result = pkgs.runCommandLocal "${executable}-checked" { } ''
    set -euo pipefail
    ${package}/bin/${executable}
    touch "$out"
  '';
  runner = pkgs.writeShellScriptBin executable ''
    set -euo pipefail
    test -f ${result}
    printf '%s\n' '${executable}: PASS (Nix-owned artifact check)'
  '';
  support = pkgs.writeTextDir "nix-support/verification-result" "${result}\n";
in
pkgs.symlinkJoin {
  name = executable;
  paths = [
    runner
    support
  ];
  passthru.verificationResult = result;
}
