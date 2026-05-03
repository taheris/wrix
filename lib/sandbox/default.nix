{
  pkgs,
  system,
  linuxPkgs,
  fenix ? null,
  treefmt ? null,
}:

let
  inherit (builtins)
    concatMap
    concatStringsSep
    elem
    mapAttrs
    attrValues
    ;

  isDarwin = system == "aarch64-darwin";
  isLinux = elem system [
    "aarch64-linux"
    "x86_64-linux"
  ];

  darwinSandbox = import ./darwin { inherit pkgs; };
  linuxSandbox = import ./linux { inherit pkgs; };

  # Profiles must use Linux packages (they contain Linux-only tools like iproute2)
  # hostPkgs is used only by profile.shellHook references.
  profiles = import ./profiles.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    inherit fenix treefmt;
  };

  # MCP server registry (uses Linux packages for server binaries)
  mcpRegistry = import ../mcp { pkgs = linuxPkgs; };

  # Claude config (~/.claude.json) - onboarding state and runtime flags
  claudeConfig = {
    bypassPermissionsModeAccepted = true;
    effortCalloutDismissed = true;
    hasCompletedOnboarding = true;
    hasSeenTasksHint = true;
    numStartups = 1;
    officialMarketplaceAutoInstallAttempted = true;
    projects = {
      "/workspace" = {
        allowedTools = [ ];
        hasTrustDialogAccepted = true;
        hasCompletedProjectOnboarding = true;
      };
    };
  };

  # Claude settings (~/.claude/settings.json) - user preferences
  # Base settings that can be extended with MCP servers
  baseClaudeSettings = {
    "$schema" = "https://json.schemastore.org/claude-code-settings.json";

    attribution = {
      commit = "";
      pr = "";
    };

    env = {
      ANTHROPIC_MODEL = "claude-opus-4-7";
      CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1";
      CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS = "1";
      DISABLE_AUTOUPDATER = "1";
      DISABLE_ERROR_REPORTING = "1";
      DISABLE_TELEMETRY = "1";
    };

    hooks = {
      Notification = [
        {
          matcher = "";
          hooks = [
            {
              type = "command";
              command = "wrapix-notify 'Claude Code' 'Waiting for input...'";
            }
          ];
        }
      ];
    };
  };

  # Build the container image using Linux packages
  # On Darwin, this will use a remote Linux builder if configured
  mkImage =
    {
      profile,
      entrypointSh,
      krunSupport ? false,
      claudePkg ? linuxPkgs.claude-code,
      claudeSettings ? baseClaudeSettings,
      mcpServerConfigs ? { },
      agent ? "claude",
      asTarball ? false,
    }:
    import ./image.nix {
      pkgs = linuxPkgs;
      inherit
        profile
        entrypointSh
        krunSupport
        claudeConfig
        claudeSettings
        mcpServerConfigs
        agent
        asTarball
        ;
      entrypointPkg = claudePkg;
    };

  # Merge extra packages/mounts/env/networkAllowlist into a profile
  extendProfile =
    profile:
    {
      packages ? [ ],
      mounts ? [ ],
      env ? { },
      networkAllowlist ? [ ],
    }:
    profile
    // {
      packages = (profile.packages or [ ]) ++ packages;
      mounts = (profile.mounts or [ ]) ++ mounts;
      env = (profile.env or { }) // env;
      networkAllowlist = (profile.networkAllowlist or [ ]) ++ networkAllowlist;
    };

  # Build MCP server configurations from the mcp attrset
  # Returns { packages, mcpServers } where:
  #   - packages: flattened list of all server runtime packages
  #   - mcpServers: attrset of server configs for claudeSettings
  buildMcpConfig =
    mcp:
    let
      # For each enabled server, look up definition and build config
      serverConfigs = mapAttrs (
        name: userConfig:
        let
          serverDef = mcpRegistry.${name} or (throw "Unknown MCP server: ${name}");
          serverConfig = serverDef.mkServerConfig userConfig;
        in
        {
          inherit (serverDef) packages;
          config = serverConfig;
        }
      ) mcp;
    in
    {
      packages = concatMap (s: s.packages) (attrValues serverConfigs);
      mcpServers = mapAttrs (_name: s: s.config) serverConfigs;
    };

  mkSandbox =
    {
      profile ? profiles.base,
      cpus ? null,
      memoryMb ? 4096,
      deployKey ? null,
      packages ? [ ],
      mounts ? [ ],
      env ? { },
      mcp ? { },
      mcpRuntime ? false,
      # Agent runtime axis composed onto the workspace profile. "claude"
      # (default) is a no-op; "pi" adds the pi-mono runtime layer.
      agent ? "claude",
      # Override the default ANTHROPIC_MODEL for this container (null = use default)
      model ? null,
    }:
    let
      # mcpRuntime: include ALL MCP server packages, defer selection to runtime.
      # Mutually exclusive with explicit mcp server config.
      effectiveMcp = if mcpRuntime then mapAttrs (_: _: { }) mcpRegistry else mcp;

      # Build MCP configuration from enabled servers
      mcpConfig = buildMcpConfig effectiveMcp;

      # Per-server config files for runtime selection (mcpRuntime only)
      mcpServerConfigs =
        if mcpRuntime then mapAttrs (name: _: mcpRegistry.${name}.mkServerConfig { }) mcpRegistry else { };

      # Extend profile with user packages + MCP server packages
      finalProfile = extendProfile profile {
        packages = packages ++ mcpConfig.packages;
        inherit mounts env;
      };

      # Merge MCP servers, profile plugins, and model override into Claude settings
      # When mcpRuntime is true, don't bake mcpServers — entrypoint handles it
      modelEnvOverride =
        if model != null then
          {
            env = baseClaudeSettings.env // {
              ANTHROPIC_MODEL = model;
            };
          }
        else
          { };

      finalClaudeSettings =
        baseClaudeSettings
        // modelEnvOverride
        // (if !mcpRuntime && mcpConfig.mcpServers != { } then { inherit (mcpConfig) mcpServers; } else { })
        // (
          if (finalProfile.enabledPlugins or { }) != { } then
            { inherit (finalProfile) enabledPlugins; }
          else
            { }
        );

      # Compute comma-separated network allowlist for WRAPIX_NETWORK=limit mode
      networkAllowlist = concatStringsSep "," (finalProfile.networkAllowlist or [ ]);

      package =
        if isLinux then
          linuxSandbox.mkSandbox {
            profile = finalProfile;
            inherit
              cpus
              memoryMb
              deployKey
              networkAllowlist
              ;
            profileImage = mkImage {
              profile = finalProfile;
              entrypointSh = ./linux/entrypoint.sh;
              krunSupport = true;
              claudeSettings = finalClaudeSettings;
              inherit agent mcpServerConfigs;
            };
          }
        else if isDarwin then
          darwinSandbox.mkSandbox {
            profile = finalProfile;
            inherit
              cpus
              memoryMb
              deployKey
              networkAllowlist
              ;
            profileImage = mkImage {
              profile = finalProfile;
              entrypointSh = ./darwin/entrypoint.sh;
              claudeSettings = finalClaudeSettings;
              asTarball = true;
              inherit agent mcpServerConfigs;
            };
          }
        else
          throw "Unsupported system: ${system}";

      # Expose the image derivation for consumers that manage containers
      # themselves (e.g. mkCity's provider runs podman directly).
      # On Linux this is a streamLayeredImage (executable script that pipes tar).
      # On Darwin this is a buildLayeredImage (tar file in store) since the
      # stream script's Linux Python shebang can't execute on macOS.
      image = mkImage {
        profile = finalProfile;
        entrypointSh =
          if isLinux then
            ./linux/entrypoint.sh
          else if isDarwin then
            ./darwin/entrypoint.sh
          else
            null;
        krunSupport = isLinux;
        asTarball = isDarwin;
        claudeSettings = finalClaudeSettings;
        inherit agent mcpServerConfigs;
      };

    in
    {
      inherit package image;
      profile = finalProfile;
    };

in
{
  inherit
    mkSandbox
    mkImage
    profiles
    baseClaudeSettings
    ;
}
