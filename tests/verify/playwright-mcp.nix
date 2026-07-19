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
  "playwright-mcp.registry-triple" = smoke "test_registry_triple_shape";

  "playwright-mcp.screenshot" = linuxHostOnly (playwrightHostAll "screenshot-test");

  "playwright-mcp.smoke" = linuxHostOnly (
    playwrightHostFunctions "smoke-test" [
      "test_network_guard_blocks_ipv4_connect"
      "test_offline_startup"
    ]
  );

}
