{ pkgs }:
let
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
  runner = import ../sandbox/fixtures/command-runner.nix { inherit pkgs; };
in
tools.overrideAttrs (old: {
  pname = "execution-fixture-conformance-native";
  nativeCheckInputs = (old.nativeCheckInputs or [ ]) ++ [
    pkgs.bash
    pkgs.coreutils
  ];
  WRIX_EXECUTION_FIXTURE_RUNNER = "${runner}/bin/test-command-runner";
  cargoTestFlags = [
    "--package"
    "wrix-verify-tools"
    "--test"
    "execution_fixtures"
  ];
  checkFlags = [ "--include-ignored" ];
})
