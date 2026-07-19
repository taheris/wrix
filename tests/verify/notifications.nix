{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  notifyTest = function: ''
    run_repo_script ${escapeShellArg "tests/standalone/notify-test.sh"} ${escapeShellArg function}
  '';
in
{
  "notifications.claude-stop-hook-config" = notifyTest "test_claude_stop_hook_config";

  "notifications.client-tcp-endpoint-override" = notifyTest "test_client_tcp_endpoint_override";

  "notifications.container-transport" = notifyTest "test_container_transport";

  "notifications.macos-tcp-bind-address" = notifyTest "test_macos_tcp_bind_address";
}
