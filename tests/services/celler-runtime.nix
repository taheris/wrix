# Exercise the same runtime package exposed to source-library consumers.
{ pkgs, wrix }:
let
  celler = wrix.rustPackage.celler;
in
assert !pkgs.lib.isDerivation celler.src;
celler.overrideAttrs {
  pname = "celler-consumer-runtime-check";
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    WRIX_TEST_CELLERD="$PWD/target/''${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release/cellerd" \
      cargoWithProfile test --offline --locked -p attic-server work_drain::test -- --include-ignored --test-threads=1
    runHook postCheck
  '';
}
