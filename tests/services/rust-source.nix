{
  pkgs,
  rustPackage,
  rustProfile,
}:

let
  inherit (rustProfile) craneLib;
  receiptProbe = rustProfile.buildPackage {
    src = ../fixtures/build-package-fixture;
    cargoLock = ../fixtures/build-package-fixture/Cargo.lock;
  };
  inherit (pkgs.lib)
    cleanSourceWith
    concatMapStringsSep
    concatStringsSep
    hasInfix
    removePrefix
    ;

  storedRoot =
    name:
    builtins.path {
      name = "source";
      path = ./source-fixture + "/${name}";
    };
  storedPackage =
    root: lock:
    rustProfile.buildPackage {
      src = root;
      cargoLock = lock;
      extraSrcs = builtins.listToAttrs (
        map
          (name: {
            inherit name;
            value = root + "/${name}";
          })
          (
            builtins.filter (name: builtins.pathExists (root + "/${name}")) [
              "crates/probe/tests"
              "crates/probe/examples"
              "tests"
            ]
          )
      );
      srcFilter = import ../../lib/services/rust-source-filter.nix {
        inherit (pkgs) lib;
        inherit (craneLib) filterCargoSources;
        inherit root;
      };
    };
  storedA = storedPackage (storedRoot "a") (storedRoot "a" + "/Cargo.lock");
  storedB = storedPackage (storedRoot "b") (storedRoot "b" + "/Cargo.lock");
  typedLock = storedPackage (storedRoot "a") (builtins.toPath (storedRoot "a" + "/Cargo.lock"));
  storedC = storedPackage (storedRoot "c") (storedRoot "c" + "/Cargo.lock");
  notesChanged = pkgs.runCommand "source" { } ''
    set -euo pipefail
    cp -r ${storedRoot "a"} "$out"
    chmod -R u+w "$out"
    printf 'Unrelated project documentation.\n' > "$out/README.md"
  '';
  storedD = storedPackage notesChanged (notesChanged + "/Cargo.lock");
  changedLock = builtins.toFile "Cargo.lock" (
    builtins.readFile ./source-fixture/a/Cargo.lock + "\n# Different lock bytes.\n"
  );
  lockChanged = storedPackage (storedRoot "a") changedLock;

  filtered =
    root:
    cleanSourceWith {
      name = "wrix-production-source-probe";
      src = root;
      filter = import ../../lib/services/rust-source-filter.nix {
        inherit (pkgs) lib;
        inherit (craneLib) filterCargoSources;
        inherit root;
      };
    };
  original = filtered ./source-fixture/a;
  fixturesChanged = filtered ./source-fixture/b;
  productionChanged = filtered ./source-fixture/c;
  leaking = cleanSourceWith {
    name = "wrix-production-source-probe";
    src = ./source-fixture/a;
    filter =
      path: type:
      removePrefix "${toString ./source-fixture/a}/" path == "lib/sandbox/network-bootstrap.sh"
      || craneLib.filterCargoSources path type;
  };

  exampleCrates = builtins.filter (name: builtins.pathExists (../../crates + "/${name}/examples")) (
    builtins.attrNames (builtins.readDir ../../crates)
  );

  resources = [
    "crates/wrix-cli/tests/fixtures/service-runtime.sh"
    "crates/wrix-cli/tests/fixtures/launch-runtime.sh"
    "crates/wrix-cli/tests/snapshots/init_help.txt"
    "crates/wrix-cli/tests/snapshots/run_help.txt"
    "crates/wrix-sandbox/tests/fixtures/consumer-entrypoint.sh"
    "crates/wrix-sandbox/tests/fixtures/container-spawn-runtime.sh"
    "crates/wrix-sandbox/tests/fixtures/lifecycle-runtime.sh"
    "crates/wrix-sandbox/tests/fixtures/podman-spawn-runtime.sh"
    ".config/nextest.toml"
    "tests/prepare-native-fixtures.sh"
    "lib/sandbox/network-bootstrap.sh"
    "lib/util/git-ssh-setup.sh"
    "tests/standalone/notify-runtime.sh"
  ];
in
assert storedRoot "a" != storedRoot "b";
assert toString storedA.bin.src == toString storedB.bin.src;
assert storedA.bin.cargoLock == storedB.bin.cargoLock;
assert storedA.bin.drvPath == typedLock.bin.drvPath;
assert storedA.cargoArtifacts.drvPath == typedLock.cargoArtifacts.drvPath;
assert storedA.bin.drvPath == storedB.bin.drvPath;
assert storedA.clippy.drvPath != storedB.clippy.drvPath;
assert storedA.nextest.drvPath != storedB.nextest.drvPath;
assert builtins.all (field: storedA.${field}.drvPath == storedD.${field}.drvPath) [
  "bin"
  "clippy"
  "nextest"
  "cargoArtifacts"
];
assert storedA.cargoArtifacts.drvPath == storedB.cargoArtifacts.drvPath;
assert storedA.bin.drvPath != storedC.bin.drvPath;
assert storedA.cargoArtifacts.drvPath == storedC.cargoArtifacts.drvPath;
assert storedA.cargoArtifacts.drvPath != lockChanged.cargoArtifacts.drvPath;
assert storedA.bin.cargoLock != lockChanged.bin.cargoLock;
assert toString original == toString fixturesChanged;
assert toString original != toString productionChanged;
assert toString original != toString leaking;
assert receiptProbe.nextest.doCheck;
assert !rustPackage.clippy.doInstallCargoArtifacts;
assert !rustPackage.nextest.doInstallCargoArtifacts;
assert rustPackage.cargoArtifacts.doInstallCargoArtifacts;
assert rustPackage.clippy.src == rustPackage.nextest.src;
assert toString rustPackage.runtimePackage.src == toString rustPackage.package.src;
assert hasInfix "--bin wrix-service-supervisor" rustPackage.runtimePackage.buildPhase;
assert hasInfix "--bin wrix-egress-gateway" rustPackage.runtimePackage.buildPhase;
assert hasInfix "--bin wrix-shared-service" rustPackage.runtimePackage.buildPhase;
assert hasInfix "--bin wrix-nix-pool-owner" rustPackage.runtimePackage.buildPhase;
pkgs.runCommand "test-rust-source-fixtures" { } ''
  set -euo pipefail
  [[ "$(${storedA.bin}/bin/source-filter-probe)" == 1 ]]
  [[ "$(${storedC.bin}/bin/source-filter-probe)" == 2 ]]
  cmp ${storedA.bin.cargoLock} ${./source-fixture/a/Cargo.lock}
  cmp ${lockChanged.bin.cargoLock} ${changedLock}
  for check in ${receiptProbe.clippy} ${receiptProbe.nextest}; do
    [[ -z "$(find "$check" -mindepth 1 -print -quit)" ]]
  done
  ${concatMapStringsSep "\n" (relative: ''
    cmp "${rustPackage.clippy.src}/${relative}" "${../../. + "/${relative}"}"
  '') resources}
  cmp ${original}/lib/sandbox/network-bootstrap.sh ${./source-fixture/a/lib/sandbox/network-bootstrap.sh}
  [[ ! -e ${original}/tests ]]
  [[ ! -e ${original}/crates/probe/tests ]]
  [[ ! -e ${original}/crates/probe/examples ]]
  [[ ! -e ${rustPackage.package.src}/tests ]]
  for crate in ${concatStringsSep " " exampleCrates}; do
    [[ ! -e "${rustPackage.package.src}/crates/$crate/examples" ]]
    diff -r "${rustPackage.clippy.src}/crates/$crate/examples" "${../../crates}/$crate/examples"
  done
  diff -r "${rustPackage.clippy.src}/crates/wrix-service/src/lifecycle/drain/native" \
    "${../../crates/wrix-service/src/lifecycle/drain/native}"
  for crate in wrix-cache wrix-cli wrix-core wrix-sandbox wrix-service; do
    [[ ! -e "${rustPackage.package.src}/crates/$crate/tests" ]]
    diff -r "${rustPackage.clippy.src}/crates/$crate/tests" "${../../crates}/$crate/tests"
  done
  mkdir "$out"
''
