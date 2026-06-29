# Playwright MCP server definition
#
# Server providing browser automation for AI-assisted frontend development
# and testing within wrix sandboxes.
#
# Exports:
#   - name: Server identifier ("playwright")
#   - packages: Runtime packages (MCP server + Chromium)
#   - mkServerConfig: Function to generate server config from user options
#   - passthru: Introspection helpers for tests and diagnostics
#
# Config options:
#   - headless: Run browser in headless mode (default: true)
#   - viewport: Default viewport size (default: { width = 1280; height = 720; })
#   - config: Passthrough to @playwright/mcp JSON config (default: {})
#
# Spec: specs/playwright-mcp.md
{ pkgs }:

let
  inherit (pkgs.lib) recursiveUpdate;

  playwrightBrowsers = pkgs.playwright-driver.browsers;
  chromiumExecutable = pkgs.runCommand "playwright-chromium-executable" { } ''
    set -euo pipefail

    mkdir -p "$out/bin"
    mapfile -t candidates < <(
      find -L "${playwrightBrowsers}" \
        -mindepth 3 \
        -maxdepth 3 \
        -path "${playwrightBrowsers}/chromium-*/*/chrome" \
        -type f \
        -perm -0100 \
        -print
    )
    if [[ "''${#candidates[@]}" -ne 1 ]]; then
      printf 'expected exactly one Chromium executable under %s, found %s\n' \
        "${playwrightBrowsers}" \
        "''${#candidates[@]}" >&2
      exit 1
    fi
    ln -s "''${candidates[0]}" "$out/bin/chrome"
  '';
  chromiumPath = "${chromiumExecutable}/bin/chrome";

  mandatoryFlags = [
    "--no-sandbox"
    "--disable-dev-shm-usage"
    "--disable-gpu"
  ];

  mkConfig =
    {
      headless,
      viewport,
      config,
    }:
    let
      configBrowser = config.browser or { };
      configContextOptions = config.contextOptions or { };
      configLaunchOptions = config.launchOptions or { };
      browserLaunchOptions = configBrowser.launchOptions or { };
      userArgs = (configLaunchOptions.args or [ ]) ++ (browserLaunchOptions.args or [ ]);
      userLaunchOptions =
        builtins.removeAttrs (recursiveUpdate configLaunchOptions browserLaunchOptions)
          [
            "args"
            "channel"
            "executablePath"
            "headless"
          ];
      userBrowser = builtins.removeAttrs configBrowser [
        "browserName"
        "launchOptions"
      ];
      userConfig = builtins.removeAttrs config [
        "browser"
        "contextOptions"
        "launchOptions"
      ];

      browser = userBrowser // {
        browserName = "chromium";
        userDataDir = configBrowser.userDataDir or "/home/wrix/.cache/playwright-mcp";
        launchOptions = userLaunchOptions // {
          args = mandatoryFlags ++ userArgs;
          channel = "chromium";
          executablePath = chromiumPath;
          inherit headless;
        };
      };
      contextOptions = configContextOptions // {
        inherit viewport;
      };
    in
    userConfig
    // {
      inherit browser contextOptions;
    };

  mkConfigFile = args: pkgs.writeText "playwright-mcp-config.json" (builtins.toJSON (mkConfig args));
in
{
  name = "playwright";

  packages = [
    pkgs.playwright-mcp
    playwrightBrowsers
    chromiumExecutable
  ];

  mkServerConfig =
    {
      headless ? true,
      viewport ? {
        width = 1280;
        height = 720;
      },
      config ? { },
    }:
    let
      configFile = mkConfigFile { inherit headless viewport config; };
    in
    {
      command = "playwright-mcp";
      args = [
        "--config"
        "${configFile}"
      ];
      env = {
        PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = "1";
        PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS = "true";
      };
    };

  passthru = {
    inherit mkConfig chromiumExecutable;
  };
}
