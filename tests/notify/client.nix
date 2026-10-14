{
  pkgs,
  linuxPkgs,
  darwinHostClient,
}:

let
  inherit (builtins)
    attrNames
    elem
    getContext
    isString
    ;
  hostClient = import ../../lib/notify/client.nix {
    inherit pkgs;
    host = true;
  };
  noGatewayPkgs = pkgs // {
    iproute2 = throw "host clients must not evaluate iproute2";
    gawk = throw "host clients must not evaluate gateway parsing";
  };
  hostDefaultGateway = import ../../lib/notify/client.nix {
    pkgs = noGatewayPkgs;
    host = true;
  };
  hostInjectedGateway = import ../../lib/notify/client.nix {
    pkgs = noGatewayPkgs;
    host = true;
    ipCommand = throw "host clients must not evaluate ipCommand";
  };
  linuxClient = import ../../lib/notify/client.nix {
    pkgs = linuxPkgs;
  };
  gatewayFixture = pkgs.writeShellScriptBin "ip" ''
    set -euo pipefail
    if [[ "$#" -ne 1 || "$1" != "route" ]]; then
      exit 64
    fi
    printf '%s\n' "$*" >> "''${WRIX_NOTIFY_TEST_IP_CALLS:?}"
    printf '%s\n' "''${WRIX_NOTIFY_TEST_ROUTES:-}"
  '';
  gatewayClient = import ../../lib/notify/client.nix {
    inherit pkgs;
    ipCommand = "${gatewayFixture}/bin/ip";
  };
in
{
  notify-host-darwin-eval =
    assert isString darwinHostClient.drvPath;
    assert hostDefaultGateway.drvPath == hostClient.drvPath;
    assert hostInjectedGateway.drvPath == hostClient.drvPath;
    assert elem linuxPkgs.iproute2.drvPath (attrNames (getContext linuxClient.text));
    pkgs.runCommand "notify-host-darwin-eval" { } ''
      set -euo pipefail
      touch "$out"
    '';

  notify-client-behavior =
    pkgs.runCommand "notify-client-behavior"
      {
        nativeBuildInputs = [ pkgs.python3 ];
        WRIX_TEST_NOTIFY_HOST = "${hostClient}/bin/wrix-notify-host";
        WRIX_TEST_NOTIFY_CLIENT = "${gatewayClient}/bin/wrix-notify";
        WRIX_TEST_IP_COMMAND = "${gatewayFixture}/bin/ip";
      }
      ''
        set -euo pipefail
        python3 ${./test_client.py}
        touch "$out"
      '';
}
