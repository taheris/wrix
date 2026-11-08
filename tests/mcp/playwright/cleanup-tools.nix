{ pkgs }:
let
  tools = import ../../../lib/util/rust-tool.nix {
    inherit pkgs;
    package = "wrix-verify-tools";
    description = "Native screenshot cleanup acceptance driver";
    extraSources = [ ../../../tests/fixtures/build-package-fixture/build.rs ];
  };
in
tools.overrideAttrs (old: {
  pname = "wrix-screenshot-cleanup-tests";
  cargoTestFlags = [
    "--package"
    "wrix-verify-tools"
    "--test"
    "screenshot_cleanup"
  ];
  postCheck = (old.postCheck or "") + ''
    cargo test --offline --locked --release --no-run --package wrix-verify-tools \
      --test screenshot_cleanup --message-format json > compiled-cleanup.json
    executable="$(${pkgs.jq}/bin/jq -r \
      'select(.reason == "compiler-artifact" and .target.name == "screenshot_cleanup" and .profile.test) | .executable' compiled-cleanup.json)"
    test -n "$executable"
    cp "$executable" "$TMPDIR/screenshot-cleanup-tests"
  '';
  postInstall = (old.postInstall or "") + ''
    install -Dm755 "$TMPDIR/screenshot-cleanup-tests" "$out/bin/screenshot-cleanup-tests"
  '';
})
