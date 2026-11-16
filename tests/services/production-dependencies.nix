{
  pkgs,
  rustProfile,
}:
let
  inherit (pkgs.lib) concatMapStringsSep genAttrs recursiveUpdate;
  inherit (rustProfile) craneLib;
  names = [
    "a"
    "b"
    "c"
    "d"
  ];
  roots = genAttrs names (
    name:
    builtins.path {
      name = "source";
      path = ./dependency-source-fixture + "/${name}";
    }
  );
  scopes = genAttrs names (
    name:
    import ../../lib/services/rust-production-dependencies.nix {
      inherit pkgs craneLib;
      root = roots.${name};
      packages = [ "fixture-app" ];
      filter = craneLib.filterCargoSources;
    }
  );
  arguments = name: {
    cargoToml = builtins.path {
      name = "Cargo.toml";
      path = roots.${name} + "/Cargo.toml";
    };
    cargoLock = builtins.path {
      name = "Cargo.lock";
      path = roots.${name} + "/Cargo.lock";
    };
    cargoExtraArgs = "--locked --package fixture-app";
  };
  artifacts = genAttrs names (
    name:
    craneLib.buildDepsOnly (
      (arguments name)
      // {
        dummySrc = scopes.${name}.src;
        cargoVendorDir = craneLib.vendorCargoDeps { inherit (arguments name) cargoLock; };
        preBuild = scopes.${name}.preBuild;
        doCheck = false;
      }
    )
  );
  products = genAttrs names (
    name:
    (rustProfile.buildPackage {
      src = roots.${name};
      inherit ((arguments name)) cargoLock;
      cargoExtraArgs = "--locked --package fixture-app";
      cargoArtifacts = artifacts.${name};
    }).bin
  );
  unexpectedVersion = (pkgs.formats.toml { }).generate "unexpected-version-Cargo.toml" (
    recursiveUpdate (builtins.fromTOML (builtins.readFile (roots.a + "/Cargo.toml"))) {
      workspace = {
        members = [
          "crates/fixture-app"
          "crates/fixture-dependency"
        ];
        package.version = "9.9.9";
      };
    }
  );
  expected = {
    a = "1";
    b = "1";
    c = "2";
    d = "3";
  };
in
assert
  scopes.a.selected == [
    "fixture-app"
    "fixture-dependency"
  ];
assert artifacts.a.drvPath == artifacts.b.drvPath;
assert artifacts.a.drvPath != artifacts.c.drvPath;
assert artifacts.a.drvPath == artifacts.d.drvPath;
assert products.a.drvPath != products.d.drvPath;
pkgs.runCommand "test-production-dependency-sources"
  {
    nativeBuildInputs = [ rustProfile.toolchain ];
  }
  ''
    set -euo pipefail
    umask 077
    export CARGO_HOME="$TMPDIR/cargo-home"
    mkdir -p "$CARGO_HOME"
    ${concatMapStringsSep "\n" (name: ''
      test "$(${products.${name}}/bin/fixture-app)" = '${expected.${name}}'
      test ! -e ${scopes.${name}.src}/crates/fixture-dev-only
      test ! -e ${scopes.${name}.src}/crates/fixture-unrelated
    '') names}
    mkdir "$TMPDIR/unexpected-version"
    cp -R ${scopes.a.src}/. "$TMPDIR/unexpected-version"
    chmod -R u+w "$TMPDIR/unexpected-version"
    cp ${unexpectedVersion} "$TMPDIR/unexpected-version/Cargo.toml"
    if (
      cd "$TMPDIR/unexpected-version"
      ${scopes.a.preBuild}
    ) > "$TMPDIR/unexpected-version.log" 2>&1; then
      echo 'A package version outside canonical Cargo.lock was accepted' >&2
      exit 1
    fi
    grep -F 'introduced a package outside canonical Cargo.lock' "$TMPDIR/unexpected-version.log"
    touch "$out"
  ''
