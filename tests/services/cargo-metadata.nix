{ pkgs, rustProfile }:
let
  source = ./source-fixture/a;
  manifest = builtins.path {
    path = source + "/Cargo.toml";
    name = "Cargo.toml";
  };
  package = rustProfile.buildPackage {
    src = source;
    cargoLock = source + "/Cargo.lock";
    extraSrcs.tests = source + "/tests";
  };
  checks = [
    package.clippy
    package.nextest
  ];
  metadata = map (check: {
    inherit (check) drvPath cargoToml cargoLock;
    cargoVendorDir = toString check.cargoVendorDir;
  }) checks;
in
assert builtins.deepSeq (map (check: check.drvPath) checks) true;
assert builtins.all (check: check.cargoToml == manifest) checks;
assert builtins.all (check: check.cargoVendorDir == package.cargoArtifacts.cargoVendorDir) checks;
assert builtins.all (check: check.src != package.bin.src) checks;
assert builtins.readFile manifest == builtins.readFile (source + "/Cargo.toml");
assert package.cargoArtifacts.cargoToml == manifest;
assert package.bin.cargoToml == manifest;
assert builtins.deepSeq metadata true;
pkgs.runCommand "cargo-canonical-metadata"
  {
    passthru = {
      inherit metadata;
      stagedSources = map (check: check.src) checks;
    };
  }
  ''
    set -euo pipefail
    touch "$out"
  ''
