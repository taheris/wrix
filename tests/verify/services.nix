{ pkgs, ... }:

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
in
{
  "services.start-loads-image-source" = lifecycle "test_service_start_loads_image_source";

  "services.temp-cache-only" = lifecycle "test_temp_cache_only_workspace_does_not_start_service";

  "services.dolt-platform-transport" = ''
    ${serviceScript "dolt-endpoints" "test_cleanup_waits_for_shutdown_writes"}
    ${serviceScript "dolt-endpoints" "test_cleanup_preserves_failure_status"}
    ${serviceScript "dolt-endpoints" "test_cleanup_preserves_skip_status"}
    ${serviceScript "dolt-endpoints" "test_cleanup_reports_server_failure"}
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

  "services.rust-helper-binaries" = serviceScript "cli-surface" "test_rust_helper_binaries";

  "services.host-nix-config" = ''
    ${hostNix "test_fake_nix_config_show_matches_real_nix"}
    ${hostNix "test_host_nix_configures_cache_and_hook"}
    ${hostNix "test_host_nix_config_fails_when_trusted_setting_ignored"}
    ${hostNix "test_host_nix_config_rejects_non_wrix_hook"}
  '';

  "services.cache-transport-http-only" = ''
    local root
    root="$(repo_root)"
    python3 "$root/tests/verify/test_services_cache_transport.py"
    python3 "$root/tests/verify/services-cache-transport.py" "$root"
  '';
}
