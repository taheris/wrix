{
  pkgs,
  focusBinary,
  nativeWrix,
}:
let
  fixture = import ../standalone/notify-fixture.nix { inherit pkgs focusBinary; };
  hostClient = import ../../lib/notify/client.nix {
    inherit pkgs;
    host = true;
  };
  focus = import ../../lib/notify/focus.nix {
    inherit pkgs;
    binary = focusBinary;
  };
  harness =
    (import ../../lib/util/rust-tool.nix {
      inherit pkgs;
      package = "wrix-notify";
      description = "Native packaged attention acceptance";
    }).overrideAttrs
      (old: {
        preBuild = (old.preBuild or "") + ''
          export NIX_BUILD_CORES=2 CARGO_BUILD_JOBS=2
          # Preserve file/line diagnostics without embedding the changing sandbox path.
          export RUSTFLAGS="''${RUSTFLAGS-} --remap-path-prefix=$NIX_BUILD_TOP=/build"
        '';
      });
  desktop = harness.overrideAttrs {
    pname = "notify-desktop-fixture";
    cargoBuildFlags = [
      "--package"
      "wrix-notify"
      "--example"
      "niri-fixture"
    ];
    cargoTestFlags = [
      "--package"
      "wrix-notify"
      "--test"
      "niri_fixture"
    ];
    checkFlags = [ "--ignored" ];
    preCheck = ''
      export WRIX_TEST_NIRI_FIXTURE="$(realpath "$(find target -path '*/release/examples/niri-fixture' -type f -print -quit)")"
      test -x "$WRIX_TEST_NIRI_FIXTURE"
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/bin"
      install -m755 "$WRIX_TEST_NIRI_FIXTURE" "$out/bin/niri-fixture"
      runHook postInstall
    '';
  };
  attention =
    assert pkgs.stdenv.hostPlatform.isLinux;
    harness.overrideAttrs {
      pname = "notify-native-attention";
      cargoBuildFlags = [
        "--package"
        "wrix-notify"
        "--test"
        "native_attention"
      ];
      cargoTestFlags = [
        "--package"
        "wrix-notify"
        "--test"
        "native_attention"
      ];
      checkFlags = [ "--ignored" ];
      nativeCheckInputs = [
        fixture.daemon
        hostClient
        focus
        pkgs.clippy
      ];
      preCheck = ''
        cargo clippy --offline --locked --release --jobs 2 --package wrix-notify --all-targets -- -D warnings
      '';
      WRIX_TEST_NIRI_FIXTURE = "${desktop}/bin/niri-fixture";
      WRIX_NATIVE_TEST_BIN = "${nativeWrix}/bin/wrix";
      installPhase = ''
        runHook preInstall
        mkdir -p "$out"
        runHook postInstall
      '';
    };
in
{
  inherit desktop attention;
}
