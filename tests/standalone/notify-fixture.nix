{ pkgs, focusBinary }:

let
  session =
    (import ../../lib/util/rust-tool.nix {
      inherit pkgs;
      package = "wrix-notify";
      scopedSource = true;
      description = "Native isolated notification daemon test session";
    }).overrideAttrs
      {
        cargoBuildFlags = [
          "--package"
          "wrix-notify"
          "--example"
          "notify-session"
        ];
        cargoTestFlags = [
          "--package"
          "wrix-notify"
          "--test"
          "notify_session"
        ];
        checkFlags = [ "--ignored" ];
        nativeCheckInputs = [ pkgs.clippy ];
        preCheck = ''
          export WRIX_TEST_NOTIFY_SESSION="$(realpath "$(find target -path '*/release/examples/notify-session' -type f -print -quit)")"
          export WRIX_TEST_SHELL="${pkgs.bash}/bin/bash"
          test -x "$WRIX_TEST_NOTIFY_SESSION"
          cargo clippy --offline --locked --release --jobs 2 --package wrix-notify --all-targets -- -D warnings
        '';
        installPhase = ''
          mkdir -p "$out/bin"
          install -m755 "$WRIX_TEST_NOTIFY_SESSION" "$out/bin/wrix-notify-test-session"
        '';
      };
  bridgeName = if pkgs.stdenv.hostPlatform.isDarwin then "terminal-notifier" else "notify-send";
  bridgeContract =
    if pkgs.stdenv.hostPlatform.isDarwin then
      ''
        if [[ "$#" -ne 4 && "$#" -ne 6 ]]; then
          exit 64
        fi
        if [[ "$1" != "-title" || "$3" != "-message" ]]; then
          exit 64
        fi
        if [[ "$#" -eq 6 && "$5" != "-sound" ]]; then
          exit 64
        fi
      ''
    else
      ''
        if [[ "$#" -ne 2 ]]; then
          exit 64
        fi
      '';
  nativeBridge = pkgs.writeShellScriptBin bridgeName ''
    set -euo pipefail

    : "''${WRIX_NOTIFY_TEST_DISPATCH_CAPTURE:?}"
    ${bridgeContract}
    if [[ -n "''${WRIX_NOTIFY_TEST_FAIL_TITLE:-}" && "${
      if pkgs.stdenv.hostPlatform.isDarwin then "$2" else "$1"
    }" == "$WRIX_NOTIFY_TEST_FAIL_TITLE" ]]; then
      exit 1
    fi
    if [[ -n "''${WRIX_NOTIFY_TEST_DISPATCH_TIME_CAPTURE:-}" ]]; then
      ${pkgs.coreutils}/bin/date +%s%N > "$WRIX_NOTIFY_TEST_DISPATCH_TIME_CAPTURE"
    fi
    ${pkgs.jq}/bin/jq -cn --args '$ARGS.positional' -- "$@" \
      >> "$WRIX_NOTIFY_TEST_DISPATCH_CAPTURE"
  '';
  tmuxBridge = pkgs.writeShellScriptBin "tmux" ''
    set -euo pipefail

    if [[ -n "''${WRIX_NOTIFY_TEST_FOCUS_STATE:-}" ]]; then
      exec "''${WRIX_NOTIFY_TEST_FOCUS_BIN:?}/tmux" "$@"
    fi
    exec ${pkgs.tmux}/bin/tmux "$@"
  '';
  daemonPkgs = pkgs // {
    tmux = tmuxBridge;
    libnotify = nativeBridge;
    terminal-notifier = nativeBridge;
  };
in
{
  inherit session;
  tmux = import ../notify/tmux.nix { inherit pkgs; };
  client = import ../../lib/notify/client.nix {
    inherit pkgs;
    # Host-side tests always provide an explicit TCP endpoint. Fail visibly if
    # one accidentally exercises Linux-only default-gateway discovery.
    ipCommand = "${pkgs.coreutils}/bin/false";
  };
  daemon = import ../../lib/notify/daemon.nix {
    pkgs = daemonPkgs;
    inherit focusBinary;
  };
}
