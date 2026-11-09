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
  storedArguments = root: lock: {
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
            ".config/nextest.toml"
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
  storedPackage = root: lock: rustProfile.buildPackage (storedArguments root lock);
  compileOnlyPackage =
    root: lock:
    rustProfile.buildPackage (
      import ../../lib/services/rust-compile-only-source.nix (storedArguments root lock)
    );
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
  configChanged = pkgs.runCommand "source" { } ''
    set -euo pipefail
    cp -r ${storedRoot "a"} "$out"
    chmod -R u+w "$out"
    printf '[profile.default]\ntest-threads = 2\n' > "$out/.config/nextest.toml"
  '';
  storedE = storedPackage configChanged (configChanged + "/Cargo.lock");
  changedLock = builtins.toFile "Cargo.lock" (
    builtins.readFile ./source-fixture/a/Cargo.lock + "\n# Different lock bytes.\n"
  );
  lockChanged = storedPackage (storedRoot "a") changedLock;
  compiledA = compileOnlyPackage (storedRoot "a") (storedRoot "a" + "/Cargo.lock");
  compiledB = compileOnlyPackage (storedRoot "b") (storedRoot "b" + "/Cargo.lock");
  compiledC = compileOnlyPackage (storedRoot "c") (storedRoot "c" + "/Cargo.lock");
  compiledE = compileOnlyPackage configChanged (configChanged + "/Cargo.lock");
  compiledLockChanged = compileOnlyPackage (storedRoot "a") changedLock;

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
  clientFiltered =
    root:
    cleanSourceWith {
      name = "wrix-client-source-probe";
      src = root;
      filter = import ../../lib/services/rust-client-source-filter.nix {
        inherit (pkgs) lib;
        inherit (craneLib) filterCargoSources;
        inherit root;
      };
    };
  clientA = clientFiltered ./source-fixture/role-a;
  clientB = clientFiltered ./source-fixture/role-b;
  clientC = clientFiltered ./source-fixture/role-c;
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
assert storedA.clippy.cargoToml == storedA.bin.cargoToml;
assert storedA.nextest.cargoToml == storedA.bin.cargoToml;
assert storedA.cargoArtifacts.cargoToml == storedA.bin.cargoToml;
assert storedA.bin.cargoToml == storedB.bin.cargoToml;
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
assert storedA.bin.drvPath == storedE.bin.drvPath;
assert storedA.cargoArtifacts.drvPath == storedE.cargoArtifacts.drvPath;
assert storedA.clippy.drvPath != storedE.clippy.drvPath;
assert storedA.nextest.drvPath != storedE.nextest.drvPath;
assert compiledA.nextest.drvPath == compiledE.nextest.drvPath;
assert compiledA.nextest.src == compiledE.nextest.src;
assert compiledA.nextest.src != storedA.nextest.src;
assert compiledA.bin.drvPath == storedA.bin.drvPath;
assert compiledA.cargoArtifacts.drvPath == storedA.cargoArtifacts.drvPath;
assert compiledA.nextest.drvPath != compiledB.nextest.drvPath;
assert compiledA.nextest.drvPath != compiledC.nextest.drvPath;
assert compiledA.cargoArtifacts.drvPath == compiledB.cargoArtifacts.drvPath;
assert compiledA.cargoArtifacts.drvPath == compiledC.cargoArtifacts.drvPath;
assert compiledA.cargoArtifacts.drvPath != compiledLockChanged.cargoArtifacts.drvPath;
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
assert rustPackage.cargoArtifacts.doCheck;
assert rustPackage.nextest.doCheck;
assert rustPackage.clippy.cargoArtifacts == rustPackage.cargoArtifacts;
assert rustPackage.nextest.cargoArtifacts == rustPackage.cargoArtifacts;
assert rustPackage.clientPackage.cargoArtifacts == rustPackage.clientCargoArtifacts;
assert rustPackage.clientCargoArtifacts != rustPackage.cargoArtifacts;
assert toString clientA == toString clientB;
assert toString clientA != toString clientC;
assert !(builtins.pathExists (clientA + "/crates/wrix-service/src/gateway/server.rs"));
assert hasInfix "cargoWithProfile check" rustPackage.cargoArtifacts.buildPhase;
assert hasInfix "--all-targets" rustPackage.cargoArtifacts.buildPhase;
assert hasInfix "cargoWithProfile test" rustPackage.cargoArtifacts.checkPhase;
assert hasInfix "--bin wrix-git-sign" rustPackage.clientPackage.buildPhase;
assert toString rustPackage.clientPackage.src != toString rustPackage.package.src;
assert
  !(builtins.pathExists (
    rustPackage.clientPackage.src + "/crates/wrix-service/src/gateway/server.rs"
  ));
assert builtins.pathExists (rustPackage.package.src + "/crates/wrix-service/src/gateway/server.rs");
assert hasInfix
  (builtins.unsafeDiscardStringContext "${rustPackage.package}/bin/wrix-host-controller")
  rustPackage.controllerPackage.buildCommand;
assert rustPackage.cacheBackendTests.cargoArtifacts == rustPackage.cargoArtifacts;
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
  [[ ! -e ${compiledA.nextest.src}/.config/nextest.toml ]]
  [[ ! -e ${rustPackage.cacheBackendTests.src}/.config/nextest.toml ]]
  diff -r ${compiledA.nextest.src}/crates/probe/tests ${./source-fixture/a/crates/probe/tests}
  diff -r ${compiledA.nextest.src}/tests ${./source-fixture/a/tests}
  ${concatMapStringsSep "\n" (relative: ''
    cmp "${rustPackage.cacheBackendTests.src}/${relative}" "${../../. + "/${relative}"}"
  '') (builtins.filter (relative: relative != ".config/nextest.toml") resources)}
  cmp ${original}/lib/sandbox/network-bootstrap.sh ${./source-fixture/a/lib/sandbox/network-bootstrap.sh}
  [[ ! -e ${original}/tests ]]
  [[ ! -e ${original}/crates/probe/tests ]]
  [[ ! -e ${original}/crates/probe/examples ]]
  [[ ! -e ${rustPackage.package.src}/tests ]]
  [[ ! -e ${rustPackage.package.src}/.config/nextest.toml ]]
  for crate in ${concatStringsSep " " exampleCrates}; do
    [[ ! -e "${rustPackage.package.src}/crates/$crate/examples" ]]
    diff -r "${rustPackage.clippy.src}/crates/$crate/examples" "${../../crates}/$crate/examples"
  done
  diff -r "${rustPackage.clippy.src}/crates/wrix-service/src/lifecycle/drain/native" \
    "${../../crates/wrix-service/src/lifecycle/drain/native}"
  diff -r "${rustPackage.clippy.src}/tests" "${../../tests}"
  diff -r "${rustPackage.cacheBackendTests.src}/tests" "${../../tests}"
  for crate in wrix-cache wrix-cli wrix-core wrix-sandbox wrix-service; do
    [[ ! -e "${rustPackage.package.src}/crates/$crate/tests" ]]
    diff -r "${rustPackage.clippy.src}/crates/$crate/tests" "${../../crates}/$crate/tests"
    diff -r "${rustPackage.cacheBackendTests.src}/crates/$crate/tests" "${../../crates}/$crate/tests"
  done
  mkdir "$out"
''
