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
  "cache.celler-packaging".script = ''
    local root
    root="$(repo_root)"
    nix build --no-link --no-warn-dirty "$root#checks.${pkgs.stdenv.hostPlatform.system}.cache-celler-packaging"
  '';

  "services.cache-devshell-overrides".script = ''
    local root
    root="$(repo_root)"
    nix eval --json --impure --no-warn-dirty --expr "import \"$root/tests/verify/cache-eval.nix\" { root = \"$root\"; system = \"${pkgs.stdenv.hostPlatform.system}\"; }" >/dev/null
  '';

  "services.rust-helper-binaries".script = serviceScript "cli-surface" "test_rust_helper_binaries";

  "services.host-nix-config".script = ''
    ${hostNix "test_fake_nix_config_show_matches_real_nix"}
    ${hostNix "test_runtime_image_inspection_and_tagging_are_stateful"}
    ${hostNix "test_host_nix_configures_cache_and_hook"}
    ${hostNix "test_host_nix_config_fails_when_trusted_setting_ignored"}
    ${hostNix "test_host_nix_config_rejects_non_wrix_hook"}
    ${hostNix "test_host_cache_opt_out_preserves_existing_nix_config"}
    ${hostNix "test_host_trust_validation_resolves_repository_and_environment_overrides"}
  '';

  "services.cache-transport-http-only".script = ''
    local root
    root="$(repo_root)"
    python3 "$root/tests/verify/test_services_cache_transport.py"
    python3 "$root/tests/verify/services-cache-transport.py" "$root"
  '';
}
