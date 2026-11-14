{ pkgs, hostCachePolicy, ... }:

let
  inherit (pkgs.lib) escapeShellArg;
  tools = import ../../lib/verify/tools.nix { inherit pkgs; };

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  hostNix = serviceScript "host-nix-config";
in
{
  "services.cache-devshell-overrides".script = ''
    local root
    root="$(repo_root)"
    nix eval --json --impure --no-warn-dirty --expr "import \"$root/tests/verify/cache-eval.nix\" { root = \"$root\"; system = \"${pkgs.stdenv.hostPlatform.system}\"; }" >/dev/null
  '';

  "services.rust-helper-binaries".script = serviceScript "cli-surface" "test_rust_helper_binaries";

  "services.host-nix-config".script = ''
    export WRIX_HOST_CACHE_POLICY_BIN="${hostCachePolicy}/bin/wrix-host-cache-policy"
    ${serviceScript "host-nix-config" "test_host_nix_hook_generations_survive_gc"}
    ${hostNix "test_fake_nix_config_show_matches_real_nix"}
    ${hostNix "test_runtime_image_inspection_and_tagging_are_stateful"}
    ${serviceScript "host-nix-config" "test_native_host_publication_hook_boundary"}
  '';

  "services.cache-transport-http-only".script = ''
    local root
    root="$(repo_root)"
    ${tools}/bin/wrix-verify-tools cache-transport "$root"
  '';
}
