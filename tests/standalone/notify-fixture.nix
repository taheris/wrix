{ pkgs }:

let
  bridgeName = if pkgs.stdenv.isDarwin then "terminal-notifier" else "notify-send";
  bridgeContract =
    if pkgs.stdenv.isDarwin then
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
    ${pkgs.jq}/bin/jq -cn --args '$ARGS.positional' -- "$@" \
      >> "$WRIX_NOTIFY_TEST_DISPATCH_CAPTURE"
  '';
  daemonPkgs = pkgs // {
    libnotify = nativeBridge;
    terminal-notifier = nativeBridge;
  };
in
{
  client = import ../../lib/notify/client.nix { inherit pkgs; };
  daemon = import ../../lib/notify/daemon.nix { pkgs = daemonPkgs; };
}
