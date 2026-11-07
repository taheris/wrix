{
  pkgs,
  rustPackage,
  rustProfile,
}:
let
  artifacts =
    pkgs.runCommand "cargo-handoff-artifacts.tar.zst"
      {
        nativeBuildInputs = [ pkgs.zstd ];
      }
      ''
        set -euo pipefail
        mkdir cached
        printf 'restored dependency\n' > cached/dependency
        tar -C cached -cf - . | zstd -o "$out"
      '';
in
pkgs.runCommand "cargo-target-handoff"
  {
    nativeBuildInputs = [
      rustProfile.craneLib.inheritCargoArtifactsHook
      pkgs.zstd
    ];
    cargoArtifacts = artifacts;
    passthru.backendTests = rustPackage.cacheBackendTests;
    prePatch = rustPackage.cacheBackendTests.prePatch;
  }
  ''
    set -euo pipefail
    # Execute the actual package handoff and Crane's actual restoration hook.
    # Isolate the output variable per case; no compiler or package is mocked.
    (
      mkdir early
      cd early
      export out="$PWD/output"
      runHook prePatch
      runHook postPatch
      test "$CARGO_TARGET_DIR" = "$out/target"
      test "$(cat "$CARGO_TARGET_DIR/dependency")" = 'restored dependency'
      test ! -e target/dependency
    )
    # The old late handoff must strand the very same restored dependency.
    (
      mkdir late
      cd late
      unset CARGO_TARGET_DIR
      export out="$PWD/output"
      runHook postPatch
      runHook prePatch
      test "$CARGO_TARGET_DIR" = "$out/target"
      test ! -e "$CARGO_TARGET_DIR/dependency"
      test "$(cat target/dependency)" = 'restored dependency'
    )
    touch "$out"
  ''
