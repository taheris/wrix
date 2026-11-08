# Immutable fixture executable for the same native test-ci consumer suite.
{ pkgs }:
let
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
in
tools.overrideAttrs (old: {
  pname = "wrix-ci-batching-tests";
  postCheck = (old.postCheck or "") + ''
    cargo test --offline --locked --release --no-run --package wrix-verify-tools --test ci_batching \
      --message-format json > compiled-ci.json
    executable="$(${pkgs.jq}/bin/jq -r \
      'select(.reason == "compiler-artifact" and .target.name == "ci_batching" and .profile.test) | .executable' compiled-ci.json)"
    test -n "$executable"
    cp "$executable" "$TMPDIR/ci-batching-tests"
  '';
  postInstall = (old.postInstall or "") + ''
    install -Dm755 "$TMPDIR/ci-batching-tests" "$out/bin/ci-batching-tests"
  '';
})
