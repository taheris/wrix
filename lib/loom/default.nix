{
  pkgs,
  crane,
  fenix,
}:

let
  inherit (pkgs) lib;

  fenixPkgs = fenix.packages.${pkgs.stdenv.hostPlatform.system};
  toolchain = fenixPkgs.combine [
    fenixPkgs.stable.defaultToolchain
  ];
  craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);

  craneFilter = craneLib.filterCargoSources;

  srcFilter =
    path: type:
    (craneFilter path type)
    || (lib.hasInfix "/loom-templates/templates/" path)
    || (lib.hasSuffix ".snap" path);

  cleanedSrc = lib.cleanSourceWith {
    src = ../../loom;
    filter = srcFilter;
  };

  commonArgs = {
    src = cleanedSrc;
    cargoLock = ../../loom/Cargo.lock;
    nativeBuildInputs = [ pkgs.git ];
    meta = {
      description = "Rust agent driver for the Wrapix workflow";
      mainProgram = "loom";
    };
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  extraSrcs = {
    "tests/loom/mock-pi" = ../../tests/loom/mock-pi;
    "tests/loom/mock-claude" = ../../tests/loom/mock-claude;
    "specs" = ../../specs;
  };

  stagedSrc = pkgs.runCommand "rust-src-with-extras" { } (
    ''
      cp -r ${cleanedSrc} $out
      chmod -R u+w $out
    ''
    + lib.concatStringsSep "\n" (
      lib.mapAttrsToList (rel: abs: ''
        mkdir -p "$(dirname "$out/${rel}")"
        cp -r ${abs} "$out/${rel}"
      '') extraSrcs
    )
  );

  bin = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      doCheck = false;
    }
  );

  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      src = stagedSrc;
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets";
    }
  );

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      src = stagedSrc;
      inherit cargoArtifacts;
      preCheck = ''
        export HOME=$(mktemp -d)
      '';
    }
  );
in
{
  inherit
    bin
    clippy
    nextest
    cargoArtifacts
    craneLib
    ;
}
