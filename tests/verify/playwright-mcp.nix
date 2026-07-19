{ pkgs, system, ... }:

let
  inherit (pkgs.lib) concatMapStringsSep escapeShellArg;

  linuxSystem =
    if system == "aarch64-darwin" then
      "aarch64-linux"
    else if system == "x86_64-darwin" then
      "x86_64-linux"
    else
      system;
  playwrightScriptFunctions = environment: script: functions: ''
    ${environment}run_repo_script ${escapeShellArg "tests/mcp/playwright/${script}.sh"} ${
      concatMapStringsSep " " escapeShellArg functions
    }
  '';
  playwrightHostFunctions = playwrightScriptFunctions "";
  playwrightCheckFunctions = playwrightScriptFunctions "PLAYWRIGHT_SYSTEM=${escapeShellArg linuxSystem} ";
  playwrightCheck = script: function: playwrightCheckFunctions script [ function ];
  playwrightHostAll = script: ''
    run_repo_script ${escapeShellArg "tests/mcp/playwright/${script}.sh"}
  '';
  linuxHostOnly =
    body:
    if pkgs.stdenv.isLinux then
      body
    else
      ''
        printf '%s\n' 'PASS: Linux-browser verifier is not applicable on this host'
      '';
  smoke = playwrightCheck "smoke-test";
in
{
  "playwright-mcp.chromium-closure" =
    playwrightCheck "build-test" "test_image_derivation_closes_over_chromium";

  "playwright-mcp.chromium-executable-path" =
    smoke "test_chromium_executable_path_derives_from_playwright_browsers";

  "playwright-mcp.mandatory-flags" = smoke "test_mandatory_flags_are_non_overridable";

  "playwright-mcp.registry-triple" = smoke "test_registry_triple_shape";

  "playwright-mcp.screenshot" = linuxHostOnly (playwrightHostAll "screenshot-test");

  "playwright-mcp.smoke" = linuxHostOnly (
    playwrightHostFunctions "smoke-test" [
      "test_network_guard_blocks_ipv4_connect"
      "test_offline_startup"
    ]
  );

  "playwright-mcp.user-options-config" =
    playwrightCheck "smoke-test" "test_user_options_reach_serialized_config";
}
