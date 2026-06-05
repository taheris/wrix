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
  chromiumRevision = pkgs.playwright-driver.passthru.browsersJSON.chromium.revision;
  chromiumPath = "${pkgs.playwright-driver.browsers}/chromium-${chromiumRevision}/chrome-linux64/chrome";

  # Flags that are always applied — cannot be overridden by user config.
  # See specs/playwright-mcp.md § Security Model for rationale.
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
      userLaunchOptions = builtins.removeAttrs (config.launchOptions or { }) [ "args" ];
      userArgs = (config.launchOptions or { }).args or [ ];
      userConfig = builtins.removeAttrs config [ "launchOptions" ];

      configJSON = {
        browser = {
          # @playwright/mcp otherwise calls createUserDataDir() which mkdirs
          # `mcp-<channel>-<cwdHash>` under playwright-core's registry path
          # (the read-only nix store) and EACCES's any tool that opens a
          # browser context (e.g. browser_take_screenshot). Pinning
          # userDataDir to a writable container path keeps the persistent
          # context inside the wrix HOME and matches the runtime layout
          # the entrypoint already provisions. See
          # playwright-core/lib/tools/mcp/browserFactory.js:createUserDataDir.
          userDataDir = "/home/wrix/.cache/playwright-mcp";
          launchOptions = {
            args = mandatoryFlags ++ userArgs;
            channel = "chromium";
            executablePath = chromiumPath;
            inherit headless;
          }
          // userLaunchOptions;
        };
        contextOptions = {
          inherit viewport;
        };
      }
      // userConfig;
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
