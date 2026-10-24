{ pkgs, system, ... }:

let
  inherit (pkgs.lib) concatMapStringsSep escapeShellArg;
  resources = import ../lib/image-resources.nix { };
  defined = script: {
    inherit script;
    inputs = resources.browserInputs;
  };

  playwrightScriptFunctions =
    script: functions:
    defined ''
      run_repo_script ${escapeShellArg "${resources.browserFixtures.relative}/playwright/${script}.sh"} ${
        concatMapStringsSep " " escapeShellArg functions
      }
    '';
  playwrightGuestFunctions =
    script: functions:
    (defined ''
      run_repo_script ${escapeShellArg "${resources.browserFixtures.relative}/playwright/darwin-guest.sh"} \
        ${escapeShellArg "${script}.sh"} ${concatMapStringsSep " " escapeShellArg functions}
    '')
    // {
      inputs = resources.runtimeInputs ++ resources.browserInputs;
    };
  playwrightFunctions =
    if pkgs.stdenv.hostPlatform.isDarwin then playwrightGuestFunctions else playwrightScriptFunctions;
  playwrightAll = script: playwrightFunctions script [ ];
in
{
  "playwright-mcp.registry-triple" = defined ''
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
