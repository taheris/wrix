{
  pkgs,
  rustProfile,
}:

let
  inherit (pkgs) lib;

  craneFilter = rustProfile.craneLib.filterCargoSources;

  # askama #[template(path = ...)] reads templates/*.md at compile time; crane's default strips them.
  # `insta` snapshot files (`crates/*/tests/snapshots/*.snap`) live next to the test sources and
  # are read at test time — the cli_help / loom-templates suites rely on them, so widen the filter
  # to keep them in nextest's view.
  srcFilter =
    path: type:
    (craneFilter path type)
    || (lib.hasInfix "/loom-templates/templates/" path)
    || (lib.hasSuffix ".snap" path);
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
