# Loom unit + integration tests as a `nix flake check` gate.
#
# Per specs/loom-tests.md (Architecture / Nix Integration), this derivation
# runs `cargo nextest run --workspace` against the loom workspace under the
# build sandbox. Container smoke (Linux-only, `nix run .#test-loom`) and
# fuzz targets (`nix run .#fuzz-loom`) are wired by their own derivations.
#
# Reuses the same fenix-pinned toolchain as lib/loom/default.nix so tests
# run against the same compiler the production binary is built with.
{
  pkgs,
  fenix,
}:

let
  fenixPkgs =
    if fenix == null then
      throw "tests/loom/default.nix: requires the fenix input"
    else
      fenix.packages.${pkgs.stdenv.hostPlatform.system};

  toolchain = fenixPkgs.stable.defaultToolchain;

  rustPlatform = pkgs.makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };

in
{
  loom-tests = rustPlatform.buildRustPackage {
    pname = "loom-tests";
    version = "0.1.0";
    src = ../../loom;

    cargoLock = {
      lockFile = ../../loom/Cargo.lock;
    };

    env = {
      HOME = "/tmp";
      # Per spec NFR §Property-Based Testing — proptest case count for CI.
      # Local exhaustive runs override via `PROPTEST_CASES=2048+`.
      PROPTEST_CASES = "32";
    };

    # Mirror lib/loom/default.nix: fixtures live outside the loom workspace
    # but are referenced from integration tests via CARGO_MANIFEST_DIR-relative
    # paths. Stage them next to the unpacked source.
    postUnpack = ''
      mkdir -p tests/loom
      cp -r ${../../tests/loom/mock-pi} tests/loom/mock-pi
      cp -r ${../../tests/loom/mock-claude} tests/loom/mock-claude
      chmod -R u+w tests/loom
    '';

    useNextest = true;
    nativeCheckInputs = [ pkgs.git ];
    cargoTestFlags = [ "--workspace" ];
    doCheck = true;

    # This derivation exists for its tests; the binary is built and
    # installed by lib/loom/default.nix. Drop the install step and emit a
    # passing-marker so the check has an output path.
    installPhase = ''
      runHook preInstall
      mkdir -p $out
      runHook postInstall
    '';

    meta = {
      description = "Loom unit + integration tests (cargo nextest --workspace)";
    };
  };
}
