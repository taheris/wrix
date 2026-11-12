{
  pkgs,
  rustProfile,
  wrix,
}:
let
  variant =
    name:
    let
      root = ./production-source-fixture + "/${name}";
      filter = import ../../lib/services/rust-source-filter.nix {
        inherit root;
        inherit (pkgs) lib;
        inherit (rustProfile.craneLib) filterCargoSources;
      };
      source = import ../../lib/services/rust-production-source.nix {
        inherit pkgs root filter;
        packages = [ "client" ];
      };
      package = rustProfile.buildPackage {
        src = root;
        cargoToml = root + "/Cargo.toml";
        cargoLock = root + "/Cargo.lock";
        srcFilter = filter;
        productionSrc = source;
        cargoExtraArgs = "--package client";
      };
    in
    {
      inherit root source package;
    };
  base = variant "base";
  unused = variant "unused-edit";
  dependency = variant "dependency-edit";
  selected = variant "selected-edit";
  projection = ".packages | map({name, targets: [.targets[] | {name, kind}] | sort_by(.name)}) | sort_by(.name)";
in
assert
  builtins.sort builtins.lessThan base.source.selected == [
    "build-helper"
    "client"
    "dependency"
    "target-helper"
  ];
assert base.source.drvPath == unused.source.drvPath;
assert base.package.bin.drvPath == unused.package.bin.drvPath;
assert base.package.nextest.drvPath != unused.package.nextest.drvPath;
assert base.package.clippy.drvPath != unused.package.clippy.drvPath;
assert base.package.bin.drvPath != dependency.package.bin.drvPath;
assert base.package.bin.drvPath != selected.package.bin.drvPath;
pkgs.runCommand "rust-production-source-isolation"
  {
    nativeBuildInputs = [
      rustProfile.toolchain
      pkgs.jq
    ];
  }
  ''
      set -euo pipefail
      export HOME="$TMPDIR/home"
      mkdir -p "$HOME"
      test "$(${base.package.bin}/bin/client)" = 7
      test "$(${unused.package.bin}/bin/client)" = 7
      test "$(${dependency.package.bin}/bin/client)" = 8
      test "$(${selected.package.bin}/bin/client)" = 9
      test ! -s ${base.source}/crates/unused/src/lib.rs
      test ! -s ${base.source}/crates/unused/tests/contract.rs
      for member in client dependency build-helper target-helper; do
      diff -r ${base.root}/crates/"$member" ${base.source}/crates/"$member"
    done
    for member in client dependency build-helper target-helper unused; do
        cmp ${base.root}/crates/"$member"/Cargo.toml ${base.source}/crates/"$member"/Cargo.toml
      done
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path ${base.root}/Cargo.toml \
        | jq -S '${projection}' > expected.json
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path ${base.source}/Cargo.toml \
        | jq -S '${projection}' > actual.json
      cmp expected.json actual.json
      mkdir -p "$TMPDIR/original"
      cp ${../../Cargo.toml} "$TMPDIR/original/Cargo.toml"
      cp ${../../Cargo.lock} "$TMPDIR/original/Cargo.lock"
      cp -r ${../../crates} "$TMPDIR/original/crates"
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path "$TMPDIR/original/Cargo.toml" \
        | jq -S '${projection}' > workspace.json
      for source in ${wrix.rustPackage.clientPackage.src} ${wrix.rustPackage.runtimePackage.src} ${wrix.rustPackage.controllerImplementation.src}; do
        cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path "$source/Cargo.toml" \
          | jq -S '${projection}' > production.json
        cmp workspace.json production.json
        test ! -s "$source/crates/wrix-verify-tools/src/lib.rs"
        test ! -s "$source/crates/wrix-image-tools/src/lib.rs"
        cmp ${../../Cargo.toml} "$source/Cargo.toml"
        cmp ${../../Cargo.lock} "$source/Cargo.lock"
      done
      touch "$out"
  ''
