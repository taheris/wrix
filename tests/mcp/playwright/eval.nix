{
  mode,
  repoRoot,
  system,
  packageName ? "",
  headless ? "true",
  width ? "1280",
  height ? "720",
  userDataDir ? "/home/wrix/.cache/playwright-mcp",
  configJson ? "{}",
}:

let
  flake = builtins.getFlake ("git+file:" + repoRoot);
  linuxSystem =
    if system == "aarch64-darwin" then
      "aarch64-linux"
    else if system == "x86_64-darwin" then
      "x86_64-linux"
    else
      system;
  pkgs = flake.inputs.nixpkgs.legacyPackages.${linuxSystem};
  wrixLib = flake.legacyPackages.${system}.lib;
  inherit (pkgs.lib) getName recursiveUpdate;

  parseBool =
    value:
    if value == "true" then
      true
    else if value == "false" then
      false
    else
      throw "headless must be true or false";
  parseInt = value: builtins.fromJSON value;

  serverDef = import ../../../lib/mcp/playwright { inherit pkgs; };
  config = recursiveUpdate (builtins.fromJSON configJson) {
    browser = {
      inherit userDataDir;
    };
  };
  mcpOptions = {
    headless = parseBool headless;
    viewport = {
      width = parseInt width;
      height = parseInt height;
    };
    inherit config;
  };
  configJSON = serverDef.passthru.mkConfig mcpOptions;
  serverConfig = serverDef.mkServerConfig mcpOptions;
  configPath = builtins.elemAt serverConfig.args 1;
  configRealizer = pkgs.runCommand "playwright-mcp-config-realizer" { } ''
    set -euo pipefail

    cp ${configPath} "$out"
  '';
  packageMatches = builtins.filter (pkg: getName pkg == packageName) serverDef.packages;
  packageByName =
    if packageMatches == [ ] then
      throw "unknown playwright package '${packageName}'"
    else
      builtins.head packageMatches;
  sandbox = wrixLib.mkSandbox {
    profile = wrixLib.profiles.base;
    mcp = {
      playwright = mcpOptions;
    };
  };
  sandboxPackageClosure = pkgs.closureInfo {
    rootPaths = sandbox.profile.packages;
  };
  outputs = {
    config-json = builtins.toJSON configJSON;
    config-path = configPath;
    config-realizer = configRealizer;
    server-args-json = builtins.toJSON serverConfig.args;
    server-command = serverConfig.command;
    server-env-json = builtins.toJSON serverConfig.env;
    server-registry-json = builtins.toJSON {
      inherit (serverDef) name;
      packageNames = map getName serverDef.packages;
      mkServerConfigIsFunction = builtins.isFunction serverDef.mkServerConfig;
      sampleConfig = {
        inherit (serverConfig) command args env;
      };
    };
    chromium-executable-path = configJSON.browser.launchOptions.executablePath;
    package = packageByName;
    package-path = toString packageByName;
    sandbox-image = sandbox.image;
    sandbox-image-path = toString sandbox.image;
    sandbox-profile-package-names-json = builtins.toJSON (map getName sandbox.profile.packages);
    sandbox-profile-package-paths-json = builtins.toJSON (map toString sandbox.profile.packages);
    sandbox-package-closure = sandboxPackageClosure;
  };
in
outputs.${mode} or (throw "unknown playwright eval mode '${mode}'")
