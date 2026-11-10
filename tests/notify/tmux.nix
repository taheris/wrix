{ pkgs }:
(import ../../lib/util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-notify";
  scopedSource = true;
  description = "Packaged notification tmux focus workflow harness";
}).overrideAttrs
  (old: {
    nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.jq ];
    nativeCheckInputs = [
      pkgs.tmux
      pkgs.clippy
    ];
    cargoTestFlags = [
      "--package"
      "wrix-notify"
      "--test"
      "notify_tmux"
    ];
    buildPhase = ''
      runHook preBuild
      cargo test --offline --locked --release --jobs 2 --package wrix-notify \
        --test notify_tmux --no-run --message-format json > compiled-tests.json
      runHook postBuild
    '';
    postCheck = ''
      cargo clippy --offline --locked --release --jobs 2 --package wrix-notify --all-targets -- -D warnings
    '';
    installPhase = ''
      runHook preInstall
      executable=$(jq -r 'select(.reason == "compiler-artifact" and .target.name == "notify_tmux" and .profile.test) | .executable' compiled-tests.json)
      test -x "$executable"
      mkdir -p "$out/bin"
      install -m755 "$executable" "$out/bin/wrix-notify-tmux-tests"
      runHook postInstall
    '';
  })
