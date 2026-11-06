{ pkgs }:
import ../util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-verify-tools";
  description = "Native Wrix verifier policy and registry audits";
  nativeCheckInputs = [ pkgs.git ];
  extraSources = [
    ../../loom.toml
    ../../tests/loom-nextest-file-targets.sh
    ../../tests/prepare-native-fixtures.sh
    ../../lib/sandbox/install-known-hosts.sh
    ../../lib/util/git-ssh-setup.sh
  ];
}
