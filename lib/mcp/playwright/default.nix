# Playwright MCP server definition
#
# Server providing browser automation for AI-assisted frontend development
# and testing within wrix sandboxes.
#
# Exports:
#   - name: Server identifier ("playwright")
#   - packages: Runtime packages (MCP server + Chromium)
#   - mkServerConfig: Function to generate server config from user options
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

  chromiumRevision = pkgs.playwright-driver.passthru.browsersJSON.chromium.revision;
  chromiumPath = "${pkgs.playwright-driver.browsers}/chromium-${chromiumRevision}/chrome-linux64/chrome";

  mandatoryFlags = [
    "--no-sandbox"
    "--disable-dev-shm-usage"
    "--disable-gpu"
  ];

  mkConfigFile =
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
      configJSON = userConfig // {
        inherit browser contextOptions;
      };
    in
    pkgs.writeText "playwright-mcp-config.json" (builtins.toJSON configJSON);
in
{
  name = "playwright";

  packages = [
    pkgs.playwright-mcp
    pkgs.playwright-driver.browsers
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
}
