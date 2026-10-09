{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath optionals;
  inherit (import ../lib/verifier.nix) darwinLive linuxLive live;
  fixture = import ../standalone/notify-fixture.nix { inherit pkgs; };
  notifyPath = makeBinPath (
    [
      fixture.client
      fixture.daemon
      pkgs.netcat
      pkgs.python3
      pkgs.socat
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
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

  nixEval = target: ''
    local root
    root="$(repo_root)"
    REPO_ROOT="$root" VERIFY_SYSTEM=${escapeShellArg system} VERIFY_TARGET=${escapeShellArg target} nix eval --raw --impure --no-warn-dirty --expr '
      import (builtins.getEnv "REPO_ROOT" + "/tests/verify/notifications-eval.nix") {
        root = builtins.getEnv "REPO_ROOT";
        system = builtins.getEnv "VERIFY_SYSTEM";
        target = builtins.getEnv "VERIFY_TARGET";
      }
    ' >/dev/null
  '';
in
{
  "notifications.claude-stop-hook-config" = nixEval "notifications.claude-stop-hook-config";

  "notifications.focus-target-envelope" = notifyTest "test_focus_target_envelope";

  "notifications.focus-target-registration" = ''
    export PATH="${notifyPath}:$PATH"
    run_repo_script_with_wrix "tests/standalone/notify-test.sh" "test_focus_target_registration"
  '';

  "notifications.client-non-blocking" = notifyTest "test_client_non_blocking";

  "notifications.client-tcp-endpoint-override" = notifyTest "test_client_tcp_endpoint_override";

  "notifications.container-transport-darwin" = darwinLive (
    notifyTest "test_container_transport_darwin"
  );

  "notifications.container-transport-linux" = linuxLive (notifyTest "test_container_transport_linux");

  "notifications.daemon-dispatch-latency" = live (notifyTest "test_daemon_dispatch_latency");

  "notifications.focus-override" = notifyTest "test_focus_override";

  "notifications.macos-tcp-bind-address" = nixEval "notifications.macos-tcp-bind-address";

  "notifications.verbose-logging" = notifyTest "test_verbose_logging";
}
