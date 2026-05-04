{
  pkgs,
  fenix ? null,
}:

let
  fenixPkgs =
    if fenix == null then
      throw "lib/loom/default.nix: requires the fenix input; pass `fenix` to this file"
    else
      fenix.packages.${pkgs.stdenv.hostPlatform.system};

  toolchain = fenixPkgs.stable.defaultToolchain;

  rustPlatform = pkgs.makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };

in
rustPlatform.buildRustPackage {
  pname = "loom";
  version = "0.1.0";
  src = ../../loom;

  cargoLock = {
    lockFile = ../../loom/Cargo.lock;
  };

  env.HOME = "/tmp";

  # Test fixtures live at /workspace/tests/loom/{mock-pi,mock-claude} —
  # outside the loom workspace src — but the integration tests in
  # crates/loom/tests/ resolve them relative to CARGO_MANIFEST_DIR via
  # `../../../tests/loom/...`. Materialize them next to the unpacked
  # source so the paths line up inside the build sandbox.
  #
  # The annotation-integrity gate in `loom/crates/loom/tests/annotations.rs`
  # also reads `specs/*.md` and `tests/loom-test.sh`; stage those at the
  # repo-relative paths the gate looks for via ancestor walk.
  postUnpack = ''
    mkdir -p tests/loom
    cp -r ${../../tests/loom/mock-pi} tests/loom/mock-pi
    cp -r ${../../tests/loom/mock-claude} tests/loom/mock-claude
    cp ${../../tests/loom-test.sh} tests/loom-test.sh
    cp -r ${../../specs} specs
    chmod -R u+w tests specs
  '';

  doCheck = true;
  nativeCheckInputs = [ pkgs.git ];

  meta = {
    description = "Rust agent driver for the Wrapix workflow";
    mainProgram = "loom";
  };
}
