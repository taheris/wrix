{ pkgs }:
import ../util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-verify-tools";
  description = "Native Wrix verifier policy and registry audits";
  extraSources = [
    ../../loom.toml
    ../../tests/loom-nextest-file-targets.sh
  ];
}
