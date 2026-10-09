{
  pkgs,
  linuxPkgs,
  wrix,
}:

let
  mkClock =
    {
      bash,
      coreutils,
      writeShellScriptBin,
      ...
    }:
    writeShellScriptBin "date" ''
      set -euo pipefail
      exec ${bash}/bin/bash ${./audit-clock.sh} ${coreutils}/bin/date "$@"
    '';
  hostClock = mkClock pkgs;
  imageClock = linuxPkgs.lib.hiPrio (mkClock linuxPkgs);
  collisionSandbox = wrix.mkSandbox {
    agent = "direct";
    agentPkg = import ../sandbox/fixtures/command-runner.nix { pkgs = linuxPkgs; };
    packages = [ imageClock ];
  };
in
{
  inherit (collisionSandbox) image;
  check =
    pkgs.runCommandLocal "audit-start-clock-conformance"
      {
        nativeBuildInputs = [
          pkgs.bash
          pkgs.coreutils
          pkgs.findutils
          pkgs.gnugrep
          pkgs.gnused
          pkgs.jq
        ];
      }
      ''
        set -euo pipefail
        export REPO_ROOT=${../..}
        ${pkgs.bash}/bin/bash "$REPO_ROOT/tests/security/audit-clock-test.sh" ${hostClock}/bin/date ${pkgs.coreutils}/bin/date
        export WRIX_TEST_AUDIT_CLOCK_DATE=${hostClock}/bin/date
        ${pkgs.bash}/bin/bash "$REPO_ROOT/tests/sandbox/entrypoint-contract.sh" test_same_second_audit_indexes_both_entrypoints
        touch "$out"
      '';
}
