{
  pkgs,
  rustProfile,
}:

let
  inherit (pkgs) lib;

  craneFilter = rustProfile.craneLib.filterCargoSources;

  # askama #[template(path = ...)] reads templates/*.md at compile time; crane's default strips them.
  srcFilter = path: type: (craneFilter path type) || (lib.hasInfix "/loom-templates/templates/" path);
in

rustProfile.buildPackage {
  src = ../../loom;
  cargoLock = ../../loom/Cargo.lock;
  inherit srcFilter;

  extraSrcs = {
    "tests/loom/mock-pi" = ../../tests/loom/mock-pi;
    "tests/loom/mock-claude" = ../../tests/loom/mock-claude;
    "tests/loom-test.sh" = ../../tests/loom-test.sh;
    "specs" = ../../specs;
  };

  nativeBuildInputs = [ pkgs.git ];

  meta = {
    description = "Rust agent driver for the Wrapix workflow";
    mainProgram = "loom";
  };
}
