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
  browser = import ../lib/browser-resources.nix { };
  containerStarts = sandboxScript "container-starts";
  entrypoint = sandboxScript "entrypoint-contract";
  network = sandboxScript "network-baseline";
  platform = sandboxScript "platform-dispatch";

in
{
  "sandbox.agent-binary-guard".script = ''
    nix build --no-link ".#checks.${system}.entrypoint-declared-runner"
  '';

  "sandbox.agent-config-homes".script = entrypoint "test_agent_config_homes_both_entrypoints";

  "sandbox.agent-lacks-net-admin" = live (network "test_agent_lacks_net_admin");

  "sandbox.custom-mounts-env".script = sandboxScriptAllWithWrix "custom-mounts-env";

  "sandbox.darwin-container-starts" = darwinLive (containerStarts "test_darwin_container_starts");

  "sandbox.darwin-image-load" = darwinLive (sandboxScriptAll "image-install-darwin-load");

  "sandbox.darwin-network-bootstrap" = darwinLive (sandboxScriptAll "darwin-network-bootstrap");

  "sandbox.entrypoint-declared-runner".script = ''
    nix build --no-link ".#checks.${system}.entrypoint-declared-runner"
  '';

  "sandbox.entrypoint-requires-bootstrap".script =
    entrypoint "test_entrypoints_require_network_bootstrap";

  "sandbox.entrypoint-deploy-key-public".script =
    entrypoint "test_deploy_key_public_derivation_both_entrypoints";

  "sandbox.entrypoint-workspace-bin-prepend".script =
    entrypoint "test_workspace_bin_path_prepend_both";

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

  "sandbox.mksandbox-api".inputs = browser.inputs;
  "sandbox.mksandbox-api".script = sandboxScriptAll "mksandbox-api";

  "sandbox.mcp-manifest-handoff".inputs = browser.inputs;
  "sandbox.mcp-manifest-handoff".script = ''
    nix build --no-link ".#checks.${system}.mcp-manifest-handoff"
  '';

  "sandbox.pi-mcp-selection".inputs = browser.inputs;
  "sandbox.pi-mcp-selection".script = ''
    nix build --no-link ".#checks.${system}.pi-mcp-native"
  '';

  "sandbox.pi-mcp-structured-results".inputs = browser.inputs;
  "sandbox.pi-mcp-structured-results".script = ''
    nix build --no-link ".#checks.${system}.pi-mcp-native"
  '';

  "sandbox.pi-mcp-images".inputs = browser.inputs;
  "sandbox.pi-mcp-images".script = ''
    nix build --no-link ".#checks.${system}.pi-mcp-native"
  '';

  "sandbox.pi-mcp-errors-cancellation".inputs = browser.inputs;
  "sandbox.pi-mcp-errors-cancellation".script = ''
    nix build --no-link ".#checks.${system}.pi-mcp-native"
  '';

  "sandbox.pi-mcp-lifecycle".inputs = browser.inputs;
  "sandbox.pi-mcp-lifecycle".script = ''
    nix build --no-link ".#checks.${system}.pi-mcp-native"
  '';

  "sandbox.network-fail-closed" = live (network "test_fail_closed");

  "sandbox.network-ipv6-blocked" = live (network "test_ipv6_blocked");

  "sandbox.network-limit-allowlist" = live (network "test_limit_allowlist");

  "sandbox.network-open-blocks-lan" = live (network "test_open_blocks_lan");

  "sandbox.nix-in-container" = linuxLive (sandboxScriptAll "nix-in-container");

  "sandbox.nix-store-verify-clean" = linuxLive (sandboxScriptAll "nix-store-verify-clean");

  "sandbox.pi-settings-precedence".script = ''
    nix build --no-link ".#checks.${system}.pi-settings-precedence"
  '';

  "sandbox.pi-codemode-tools".script = ''
    nix build --no-link ".#checks.${system}.pi-codemode-tools"
  '';

  "sandbox.platform-dispatch".script = platform "test_platform_dispatch_current_system";

  "sandbox.uid-mapping" = linuxLive (sandboxScriptAll "uid-mapping");

  "sandbox.unsupported-system-error".script = platform "test_unsupported_system_error";

  "sandbox.unsafe-podman-socket" = requiring linux [ ] (
    sandboxScriptAllWithWrix "unsafe-podman-socket"
  );

  "sandbox.workspace-bin-path-absent" = linuxLive (sandboxScriptAll "workspace-bin-path");

  "sandbox.workspace-bin-path-present" = linuxLive (sandboxScriptAll "workspace-bin-path");

}
