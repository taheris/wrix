{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath optionalString;

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
  darwinOnly =
    body:
    if pkgs.stdenv.hostPlatform.isDarwin then
      body
    else
      ''
        printf '%s\n' 'SKIP: Darwin-only verifier is not applicable on this host' >&2
        exit 77
      '';
  linuxOnly =
    body:
    if pkgs.stdenv.hostPlatform.isLinux then
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

  "sandbox.darwin-container-starts" = darwinOnly (containerStarts "test_darwin_container_starts");

  "sandbox.darwin-image-load" = darwinOnly (sandboxScriptAll "image-install-darwin-load");

  "sandbox.darwin-network-bootstrap" = darwinOnly (sandboxScriptAll "darwin-network-bootstrap");

  "sandbox.entrypoint-agent-dispatch" = entrypoint "test_agent_dispatch_both_entrypoints";

  "sandbox.entrypoint-requires-bootstrap" = entrypoint "test_entrypoints_require_network_bootstrap";

  "sandbox.entrypoint-deploy-key-public" =
    entrypoint "test_deploy_key_public_derivation_both_entrypoints";

  "sandbox.entrypoint-workspace-bin-prepend" = entrypoint "test_workspace_bin_path_prepend_both";

  "sandbox.filesystem-isolation" = sandboxScriptAll "filesystem-isolation";

  "sandbox.linux-container-starts" = containerStarts "test_linux_container_starts";

  "sandbox.linux-microvm-missing-kvm" = linuxOnly (
    sandboxScriptWithWrix "rust-launcher-live" "test_linux_microvm_missing_kvm_fails_before_podman"
  );

  "sandbox.linux-microvm-runtime" = linuxOnly (sandboxScriptAll "microvm-runtime");

  "sandbox.linux-network-bootstrap" = ''
    ${optionalString pkgs.stdenv.hostPlatform.isLinux ''
      export PATH="${
        makeBinPath [
          pkgs.diffutils
          pkgs.getent.provider
          pkgs.iptables
          pkgs.libcap
          pkgs.netcat
          pkgs.nftables
          pkgs.stdenv.cc
          pkgs.util-linux
        ]
      }:$PATH"
    ''}
    ${sandboxScriptAll "network-bootstrap"}
  '';

  "sandbox.mksandbox-api" = sandboxScriptAll "mksandbox-api";

  "sandbox.mcp-agent-adapters" = sandboxScriptAll "mcp-agent-adapters";

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
