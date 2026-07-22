{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath optionals;
  fixture = import ../standalone/notify-fixture.nix { inherit pkgs; };
  notifyPath = makeBinPath (
    [
      fixture.client
      fixture.daemon
      pkgs.netcat
      pkgs.socat
    ]
    ++ optionals pkgs.stdenv.isLinux [
      pkgs.podman
      pkgs.shadow
      pkgs.skopeo
      pkgs.util-linux
    ]
  );

  notifyTest = function: ''
    export PATH="${notifyPath}:$PATH"
    run_repo_script ${escapeShellArg "tests/standalone/notify-test.sh"} ${escapeShellArg function}
  '';
in
{
  "notifications.claude-stop-hook-config" = notifyTest "test_claude_stop_hook_config";

  "notifications.client-tcp-endpoint-override" = notifyTest "test_client_tcp_endpoint_override";

  "notifications.container-transport" = notifyTest "test_container_transport";

  "notifications.macos-tcp-bind-address" = notifyTest "test_macos_tcp_bind_address";
}
