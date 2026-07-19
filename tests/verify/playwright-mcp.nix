{ pkgs, ... }:

let
  inherit (pkgs.lib) concatMapStringsSep escapeShellArg;

  playwrightScriptFunctions = script: functions: ''
    run_repo_script ${escapeShellArg "tests/mcp/playwright/${script}.sh"} ${
      concatMapStringsSep " " escapeShellArg functions
    }
  '';
  playwrightScript = script: function: playwrightScriptFunctions script [ function ];
  playwrightScriptAll = script: ''
    run_repo_script ${escapeShellArg "tests/mcp/playwright/${script}.sh"}
  '';
  smoke = playwrightScript "smoke-test";
in
{
  "playwright-mcp.chromium-closure" = playwrightScriptAll "build-test";

  "playwright-mcp.chromium-executable-path" =
    smoke "test_chromium_executable_path_derives_from_playwright_browsers";

  "playwright-mcp.mandatory-flags" = smoke "test_mandatory_flags_are_non_overridable";

  "playwright-mcp.registry-triple" = smoke "test_registry_triple_shape";

  "playwright-mcp.screenshot" = playwrightScriptAll "screenshot-test";

  "playwright-mcp.smoke" = playwrightScriptFunctions "smoke-test" [
    "test_network_guard_blocks_ipv4_connect"
    "test_offline_startup"
  ];

  "playwright-mcp.user-options-config" = playwrightScriptFunctions "smoke-test" [
    "test_user_options_reach_serialized_config"
    "test_viewport_option_reaches_live_browser"
  ];
}
