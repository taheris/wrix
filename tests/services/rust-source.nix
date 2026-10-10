{ pkgs, rustPackage }:

let
  inherit (pkgs.lib) concatMapStringsSep;

  resources = [
    "crates/wrix-cli/tests/fixtures/launch-runtime.sh"
    "crates/wrix-cli/tests/snapshots/init_help.txt"
    "crates/wrix-cli/tests/snapshots/run_help.txt"
    "crates/wrix-sandbox/tests/fixtures/consumer-entrypoint.sh"
    "crates/wrix-sandbox/tests/fixtures/container-spawn-runtime.sh"
    "crates/wrix-sandbox/tests/fixtures/lifecycle-runtime.sh"
    "crates/wrix-sandbox/tests/fixtures/podman-spawn-runtime.sh"
    "lib/util/git-ssh-setup.sh"
    "tests/standalone/notify-runtime.sh"
  ];
in
assert rustPackage.clippy.src == rustPackage.nextest.src;
pkgs.runCommand "test-rust-source-fixtures" { } ''
  set -euo pipefail
  ${concatMapStringsSep "\n" (relative: ''
    cmp "${rustPackage.clippy.src}/${relative}" "${../../. + "/${relative}"}"
  '') resources}
  mkdir "$out"
''
