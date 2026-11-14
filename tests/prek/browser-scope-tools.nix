{ pkgs }:
let
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };
in
tools.overrideAttrs (old: {
  pname = "wrix-browser-scope-tests";
  postCheck = (old.postCheck or "") + ''
    cargo test --offline --locked --release --no-run --package wrix-verify-tools --test browser_gate_scope \
      --message-format json > browser-scope-compiled.json
    executable="$(${pkgs.jq}/bin/jq -r \
      'select(.reason == "compiler-artifact" and .target.name == "browser_gate_scope" and .profile.test) | .executable' browser-scope-compiled.json)"
    test -n "$executable"
    cp "$executable" "$TMPDIR/browser-gate-scope-tests"
  '';
  postInstall = (old.postInstall or "") + ''
    install -Dm755 "$TMPDIR/browser-gate-scope-tests" "$out/bin/browser-gate-scope-tests"
  '';
})
