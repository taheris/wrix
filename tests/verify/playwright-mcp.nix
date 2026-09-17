{ pkgs, system, ... }:

let
  inherit (pkgs.lib) concatMapStringsSep escapeShellArg;

  playwrightScriptFunctions = script: functions: ''
    run_repo_script ${escapeShellArg "tests/mcp/playwright/${script}.sh"} ${
      concatMapStringsSep " " escapeShellArg functions
    }
  '';
  playwrightGuestFunctions = script: functions: ''
    run_repo_script ${escapeShellArg "tests/mcp/playwright/darwin-guest.sh"} \
      ${escapeShellArg "${script}.sh"} ${concatMapStringsSep " " escapeShellArg functions}
  '';
  playwrightFunctions =
    if pkgs.stdenv.hostPlatform.isDarwin then playwrightGuestFunctions else playwrightScriptFunctions;
  playwrightAll = script: playwrightFunctions script [ ];
in
{
  "playwright-mcp.registry-triple" = ''
    local root
    root="$(repo_root)"
    nix build --no-link --no-warn-dirty \
      "$root#legacyPackages.${system}.testApps.playwright-mcp-registry"
  '';

  "playwright-mcp.screenshot" = playwrightAll "screenshot-test";

  "playwright-mcp.smoke" = playwrightFunctions "smoke-test" [
    "test_network_guard_blocks_ipv4_connect"
    "test_offline_startup"
  ];
}
