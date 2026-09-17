{ pkgs }:

let
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
    if [[ -n "''${WRIX_NOTIFY_TEST_DISPATCH_TIME_CAPTURE:-}" ]]; then
      ${pkgs.coreutils}/bin/date +%s%N > "$WRIX_NOTIFY_TEST_DISPATCH_TIME_CAPTURE"
    fi
    ${pkgs.jq}/bin/jq -cn --args '$ARGS.positional' -- "$@" \
      >> "$WRIX_NOTIFY_TEST_DISPATCH_CAPTURE"
  '';
  daemonPkgs = pkgs // {
    libnotify = nativeBridge;
    terminal-notifier = nativeBridge;
  };
in
{
  client = import ../../lib/notify/client.nix {
    inherit pkgs;
    # Host-side tests always provide an explicit TCP endpoint. Fail visibly if
    # one accidentally exercises Linux-only default-gateway discovery.
    ipCommand = "${pkgs.coreutils}/bin/false";
  };
  daemon = import ../../lib/notify/daemon.nix { pkgs = daemonPkgs; };
}
