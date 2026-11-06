{ pkgs }:
import ../util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-verify-tools";
  description = "Native Wrix verifier policy and registry audits";
  nativeCheckInputs = [
    pkgs.git
    pkgs.jq
  ];
  extraSources = [
    ../../loom.toml
    ../../tests/loom-nextest-file-targets.sh
    ../../tests/prepare-native-fixtures.sh
    ../../lib/sandbox/install-known-hosts.sh
    ../../lib/util/git-ssh-setup.sh
    ../../lib/services/nix-pool/bootstrap-process.sh
    ../../tests/fixtures/build-package-fixture/build.rs
    ../../tests/sandbox/entrypoint-contract.sh
    ../../lib/sandbox/linux/entrypoint.sh
    ../../lib/sandbox/darwin/entrypoint.sh
    ../../lib/sandbox/network-ready.sh
    ../../lib/sandbox/mcp-manifest.sh
    ../../lib/sandbox/shared-nix-client.sh
    ../../lib/sandbox/tool-cache.sh
    ../../lib/beads/sandbox.sh
  ];
}
