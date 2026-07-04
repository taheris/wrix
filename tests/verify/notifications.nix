{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  notifyTest = function: ''
    local build_dir
    local root
    local runner
    local status
    root="$(repo_root)"
    build_dir="$(mktemp -d -t wrix-verify-notify.XXXXXX)"
    nix build --out-link "$build_dir/result" --no-warn-dirty "$root#legacyPackages.${system}.testApps.test-notify"
    runner="$(readlink -f "$build_dir/result")"
    status=0
    "$runner/bin/test-notify" ${escapeShellArg function} || status="$?"
    rm -rf "$build_dir"
    return "$status"
  '';
in
{
  "notifications.claude-stop-hook-config" = notifyTest "test_claude_stop_hook_config";

  "notifications.client-tcp-endpoint-override" = notifyTest "test_client_tcp_endpoint_override";

  "notifications.container-transport" = notifyTest "test_container_transport";

  "notifications.macos-tcp-bind-address" = notifyTest "test_macos_tcp_bind_address";
}
