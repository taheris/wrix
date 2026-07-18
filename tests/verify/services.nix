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

  "services.image-labels" = lifecycle "test_service_image_labels";

  "services.rust-helper-binaries" = serviceScript "cli-surface" "test_rust_helper_binaries";

  "services.host-nix-config" = ''
    ${hostNix "test_host_nix_configures_cache_and_hook"}
    ${hostNix "test_host_nix_config_fails_when_trusted_setting_ignored"}
    ${hostNix "test_host_nix_config_rejects_non_wrix_hook"}
  '';

  "services.limit-mode-cache-endpoint" = sandboxNix "test_limit_mode_cache_endpoint";

  "services.cache-transport-http-only" = ''
    local root
    root="$(repo_root)"
    python3 "$root/tests/verify/services-cache-transport.py" "$root"
  '';
}
