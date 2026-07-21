{ pkgs, system }:

let
  inherit (pkgs.lib) escapeShellArg;

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  serviceScriptWithWrix = script: function: ''
    run_repo_script_with_wrix ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  lifecycle = serviceScriptWithWrix "lifecycle";
  hostNix = serviceScriptWithWrix "host-nix-config";
  sandboxNix = serviceScript "sandbox-nix-config";
  dolt = serviceScriptWithWrix "dolt-endpoints";
  linuxSystem =
    if system == "aarch64-darwin" then
      "aarch64-linux"
    else if system == "x86_64-darwin" then
      "x86_64-linux"
    else
      system;
in
{
  "services.devshell-start-independent" =
    if pkgs.stdenv.isLinux then
      ''
        local root
        root="$(repo_root)"
        nix build --no-link --no-warn-dirty \
          "$root#legacyPackages.${system}.systemTests.services-devshell-start-independent"
      ''
    else
      hostNix "test_mkdevshell_nix_cache";

  "services.start-loads-image-source" = lifecycle "test_service_start_loads_image_source";

  "services.temp-cache-only" = lifecycle "test_temp_cache_only_workspace_does_not_start_service";

  "services.dolt-platform-transport" = ''
    ${dolt "test_linux_dolt_uses_workspace_socket"}
    ${dolt "test_explicit_tcp_dolt_uses_loopback_tcp"}
  '';

  "services.cache-state-layout" = ''
    ${hostNix "test_default_cache_state_layout"}
    ${hostNix "test_mkdevshell_nix_cache"}
  '';

  "services.container-pull-config" = sandboxNix "test_container_pull_config";

  "services.cache-http-endpoint" = sandboxNix "test_no_container_dns_dependency";

  "services.sandbox-cache-boundary" = sandboxNix "test_no_host_store_or_cache_secret";

  "services.image-labels" = lifecycle "test_service_image_labels";

  "services.rust-helper-binaries" = serviceScript "cli-surface" "test_rust_helper_binaries";

  "services.host-nix-config" = ''
    ${hostNix "test_host_nix_configures_cache_and_hook"}
    ${hostNix "test_host_nix_config_fails_when_trusted_setting_ignored"}
    ${hostNix "test_host_nix_config_rejects_non_wrix_hook"}
  '';

  "services.limit-mode-cache-endpoint" = ''
    local root
    root="$(repo_root)"
    nix build --no-link --no-warn-dirty \
      "$root#legacyPackages.${linuxSystem}.systemTests.services-cache-network"
  '';

  "services.cache-transport-http-only" = ''
    local root
    root="$(repo_root)"
    python3 "$root/tests/verify/services-cache-transport.py" "$root"
  '';
}
