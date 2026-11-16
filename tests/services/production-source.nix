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
      package = rustProfile.buildPackage (
        {
          src = root;
          cargoToml = root + "/Cargo.toml";
          cargoLock = root + "/Cargo.lock";
          srcFilter = filter;
          extraSrcs = builtins.listToAttrs (
            map
              (member: {
                name = "crates/${member}/tests";
                value = root + "/crates/${member}/tests";
              })
              (
                builtins.filter (member: builtins.pathExists (root + "/crates/${member}/tests")) (
                  builtins.attrNames (builtins.readDir (root + "/crates"))
                )
              )
          );
          productionSrc = source;
          cargoExtraArgs = "--package client";
        }
        // pkgs.lib.optionalAttrs (name != "base") {
          # Fixture manifests are identical; mirror the production dependency-cache boundary.
          cargoArtifacts = base.package.cargoArtifacts;
        }
      );
    in
    {
      inherit root source package;
    };
  base = variant "base";
  unused = variant "unused-edit";
  dependency = variant "dependency-edit";
  selected = variant "selected-edit";
  targets = variant "target-edit";
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
assert base.source.drvPath == targets.source.drvPath;
assert base.package.bin.drvPath == targets.package.bin.drvPath;
assert base.package.clippy.drvPath != targets.package.clippy.drvPath;
assert base.package.nextest.drvPath != targets.package.nextest.drvPath;
pkgs.runCommand "rust-production-source-isolation"
  {
    nativeBuildInputs = [
      rustProfile.toolchain
      pkgs.stdenv.cc
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
      test ! -e ${base.source}/crates/unused/tests/contract.rs
      test ! -e ${targets.source}/crates/client/tests/new-contract.rs
      test ! -e ${targets.source}/crates/unused/src/bin/new-tool.rs
      cmp ${targets.root}/crates/client/tests/new-contract.rs ${targets.package.nextest.src}/crates/client/tests/new-contract.rs
      cmp ${targets.root}/crates/unused/src/bin/new-tool.rs ${targets.package.clippy.src}/crates/unused/src/bin/new-tool.rs
      cp -r ${targets.package.nextest.src} "$TMPDIR/target-fixture"
      chmod -R u+w "$TMPDIR/target-fixture"
      cargo test --offline --locked --manifest-path "$TMPDIR/target-fixture/Cargo.toml" --test new-contract
      printf '\n#[test]\nfn failing_added_contract_is_not_lost() { panic!("new contract failure"); }\n' >> "$TMPDIR/target-fixture/crates/client/tests/new-contract.rs"
      if cargo test --offline --locked --manifest-path "$TMPDIR/target-fixture/Cargo.toml" --test new-contract > failure.log 2>&1; then
        echo 'new development target failure was lost' >&2
        exit 1
      fi
      grep -F 'new contract failure' failure.log
      for member in client dependency build-helper target-helper; do
      diff -r ${base.root}/crates/"$member" ${base.source}/crates/"$member"
    done
    for member in client dependency build-helper target-helper unused; do
        cmp ${base.root}/crates/"$member"/Cargo.toml ${base.source}/crates/"$member"/Cargo.toml
      done
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path ${base.root}/Cargo.toml \
        | jq -S '${projection}' > expected.json
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path ${base.package.nextest.src}/Cargo.toml \
        | jq -S '${projection}' > actual.json
      cmp expected.json actual.json
      mkdir -p "$TMPDIR/original"
      cp ${../../Cargo.toml} "$TMPDIR/original/Cargo.toml"
      cp ${../../Cargo.lock} "$TMPDIR/original/Cargo.lock"
      cp -r ${../../crates} "$TMPDIR/original/crates"
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path "$TMPDIR/original/Cargo.toml" \
        | jq -S '${projection}' > workspace.json
      cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path ${wrix.rustPackage.clippy.src}/Cargo.toml \
        | jq -S '${projection}' > verification.json
      cmp workspace.json verification.json
      for source in ${wrix.rustPackage.clientPackage.src} ${wrix.rustPackage.runtimePackage.src} ${wrix.rustPackage.controllerImplementation.src}; do
        cargo metadata --offline --locked --no-deps --format-version 1 --manifest-path "$source/Cargo.toml" > production.json
        for manifest in "$TMPDIR/original"/crates/*/Cargo.toml; do
          member="$(basename "$(dirname "$manifest")")"
          cmp "$manifest" "$source/crates/$member/Cargo.toml"
        done
        test ! -s "$source/crates/wrix-verify-tools/src/lib.rs"
        test ! -s "$source/crates/wrix-image-tools/src/lib.rs"
        cmp ${../../Cargo.toml} "$source/Cargo.toml"
        cmp ${../../Cargo.lock} "$source/Cargo.lock"
      done
      touch "$out"
  ''
