{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  sandboxScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/sandbox/${script}.sh"} ${escapeShellArg function}
  '';
  sandboxScriptAll = script: ''
    run_repo_script ${escapeShellArg "tests/sandbox/${script}.sh"}
  '';
  sandboxScriptWithWrix = script: function: ''
    run_repo_script_with_wrix ${escapeShellArg "tests/sandbox/${script}.sh"} ${escapeShellArg function}
  '';
  sandboxScriptAllWithWrix = script: ''
    run_repo_script_with_wrix ${escapeShellArg "tests/sandbox/${script}.sh"}
  '';
  containerStarts = sandboxScript "container-starts";
  entrypoint = sandboxScript "entrypoint-contract";
  network = sandboxScript "network-baseline";
  platform = sandboxScript "platform-dispatch";
  linuxOnly =
    body:
    if pkgs.stdenv.isLinux then
      body
    else
      ''
        printf '%s\n' 'PASS: Linux-only verifier is not applicable on this host'
      '';
in
{
  "sandbox.agent-binary-guard" = sandboxScriptAll "agent-binary-guard";

  "sandbox.agent-config-homes" = entrypoint "test_agent_config_homes_both_entrypoints";

  "sandbox.agent-lacks-net-admin" = network "test_agent_lacks_net_admin";

  "sandbox.custom-mounts-env" = sandboxScriptAllWithWrix "custom-mounts-env";

  "sandbox.darwin-container-starts" = containerStarts "test_darwin_container_starts";

  "sandbox.darwin-image-load" = sandboxScriptAll "image-install-darwin-load";

  "sandbox.darwin-network-bootstrap" = sandboxScriptAll "darwin-network-bootstrap";

  "sandbox.entrypoint-agent-dispatch" = entrypoint "test_agent_dispatch_both_entrypoints";

  "sandbox.entrypoint-workspace-bin-prepend" = entrypoint "test_workspace_bin_path_prepend_both";

  "sandbox.filesystem-isolation" = sandboxScriptAll "filesystem-isolation";

  "sandbox.linux-container-starts" = containerStarts "test_linux_container_starts";

  "sandbox.linux-microvm-runtime" = linuxOnly (
    sandboxScriptWithWrix "rust-launcher-live" "test_linux_microvm_runtime"
  );

  "sandbox.mksandbox-api" =
    sandboxScript "mksandbox-api" "test_mksandbox_accepts_documented_parameters";

  "sandbox.network-fail-closed" = network "test_fail_closed";

  "sandbox.network-ipv6-blocked" = network "test_ipv6_blocked";

  "sandbox.network-limit-allowlist" = network "test_limit_allowlist";

  "sandbox.network-open-blocks-lan" = network "test_open_blocks_lan";

  "sandbox.nix-in-container" = sandboxScriptAll "nix-in-container";

  "sandbox.nix-store-verify-clean" = sandboxScriptAll "nix-store-verify-clean";

  "sandbox.platform-dispatch" = platform "test_platform_dispatch_current_system";

  "sandbox.uid-mapping" = sandboxScriptAll "uid-mapping";

  "sandbox.unsupported-system-error" = platform "test_unsupported_system_error";

  "sandbox.unsafe-podman-socket" = sandboxScriptAllWithWrix "unsafe-podman-socket";

  "sandbox.workspace-bin-path-absent" = sandboxScriptAll "workspace-bin-path";

  "sandbox.workspace-bin-path-present" = sandboxScriptAll "workspace-bin-path";

}
