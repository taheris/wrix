{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath optionalString;
  inherit (import ../lib/verifier.nix)
    darwinLive
    linux
    linuxLive
    live
    requiring
    ;

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

in
{
  "sandbox.agent-binary-guard" = ''
    nix build --no-link ".#checks.${system}.entrypoint-declared-runner"
  '';

  "sandbox.agent-config-homes" = entrypoint "test_agent_config_homes_both_entrypoints";

  "sandbox.agent-lacks-net-admin" = live (network "test_agent_lacks_net_admin");

  "sandbox.custom-mounts-env" = sandboxScriptAllWithWrix "custom-mounts-env";

  "sandbox.darwin-container-starts" = darwinLive (containerStarts "test_darwin_container_starts");

  "sandbox.darwin-image-load" = darwinLive (sandboxScriptAll "image-install-darwin-load");

  "sandbox.darwin-network-bootstrap" = darwinLive (sandboxScriptAll "darwin-network-bootstrap");

  "sandbox.entrypoint-declared-runner" = ''
    nix build --no-link ".#checks.${system}.entrypoint-declared-runner"
  '';

  "sandbox.entrypoint-requires-bootstrap" = entrypoint "test_entrypoints_require_network_bootstrap";

  "sandbox.entrypoint-deploy-key-public" =
    entrypoint "test_deploy_key_public_derivation_both_entrypoints";

  "sandbox.entrypoint-workspace-bin-prepend" = entrypoint "test_workspace_bin_path_prepend_both";

  "sandbox.filesystem-isolation" = live (sandboxScriptAll "filesystem-isolation");

  "sandbox.linux-container-starts" = linuxLive (containerStarts "test_linux_container_starts");

  "sandbox.linux-microvm-missing-kvm" = requiring linux [ ] (
    sandboxScriptWithWrix "rust-launcher-live" "test_linux_microvm_missing_kvm_fails_before_podman"
  );

  "sandbox.linux-microvm-runtime" = requiring linux [ "kvm" "container-runtime" ] (
    sandboxScriptAll "microvm-runtime"
  );

  "sandbox.linux-network-bootstrap" = requiring linux [ "user-network-namespace" ] ''
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

  "sandbox.mcp-agent-adapters" = ''
    export PATH="${import ../../lib/sandbox/pi.nix { inherit pkgs; }}/bin:$PATH"
    ${sandboxScriptAll "mcp-agent-adapters"}
  '';

  "sandbox.network-fail-closed" = live (network "test_fail_closed");

  "sandbox.network-ipv6-blocked" = live (network "test_ipv6_blocked");

  "sandbox.network-limit-allowlist" = live (network "test_limit_allowlist");

  "sandbox.network-open-blocks-lan" = live (network "test_open_blocks_lan");

  "sandbox.nix-in-container" = linuxLive (sandboxScriptAll "nix-in-container");

  "sandbox.nix-store-verify-clean" = linuxLive (sandboxScriptAll "nix-store-verify-clean");

  "sandbox.pi-settings-precedence" = ''
    nix build --no-link ".#checks.${system}.pi-settings-precedence"
  '';

  "sandbox.pi-codemode-tools" = ''
    nix build --no-link ".#checks.${system}.pi-codemode-tools"
  '';

  "sandbox.platform-dispatch" = platform "test_platform_dispatch_current_system";

  "sandbox.uid-mapping" = linuxLive (sandboxScriptAll "uid-mapping");

  "sandbox.unsupported-system-error" = platform "test_unsupported_system_error";

  "sandbox.unsafe-podman-socket" = requiring linux [ ] (
    sandboxScriptAllWithWrix "unsafe-podman-socket"
  );

  "sandbox.workspace-bin-path-absent" = linuxLive (sandboxScriptAll "workspace-bin-path");

  "sandbox.workspace-bin-path-present" = linuxLive (sandboxScriptAll "workspace-bin-path");

}
