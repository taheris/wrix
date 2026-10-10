{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  serviceScriptWithWrix = script: function: ''
    run_repo_script_with_wrix ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  hostNix = serviceScriptWithWrix "host-nix-config";
in
{
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
