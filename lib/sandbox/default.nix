{
  pkgs,
  system,
  linuxPkgs,
  crane ? null,
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

  isDarwin = elem system [
    "aarch64-darwin"
    "x86_64-darwin"
  ];
  isLinux = elem system [
    "aarch64-linux"
    "x86_64-linux"
  ];

  darwinSandbox = import ./darwin { inherit pkgs; };
  linuxSandbox = import ./linux { inherit pkgs; };

  manifest = import ./manifest.nix { inherit pkgs; };
  imageTagLib = import ../util/image-tag.nix { };

  # podman accepts `localhost/<name>:<tag>` refs; Apple's `container` CLI
  # uses bare `<name>:<tag>`. Match the convention each launcher expects.
  imageRefPrefix = if isDarwin then "" else "localhost/";

  mkImageRef = image: "${imageRefPrefix}${image.imageName}:${imageTagLib.mkImageTag image}";

  # Profiles must use Linux packages (they contain Linux-only tools like iproute2)
  # for the image-side surface; hostPkgs governs the toolchain that backs
  # profile.toolchain, the devshell PATH prepend, and buildPackage's craneLib.
  profilesModule = import ./profiles.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    inherit crane fenix treefmt;
  };
  # rustProfileFromFile is the internal constructor that powers
  # `wrapix.rustProfile` (lib/default.nix); it is intentionally stripped from
  # the public `profiles` surface — consumers reach pinned rust profiles
  # through `wrapix.rustProfile { toolchain; sha256; }`.
  profiles = builtins.removeAttrs profilesModule [ "rustProfileFromFile" ];
  inherit (profilesModule) rustProfileFromFile;

  # Separate profile instance whose buildPackage targets the image platform
  # (linuxPkgs). Used to construct the in-image MCP server binaries that get
  # baked into sandbox images; profilesModule.rust's buildPackage is host-platform
  # and would ship a non-runnable binary into a Linux image on Darwin hosts.
  imageProfilesModule = import ./profiles.nix {
    pkgs = linuxPkgs;
    hostPkgs = linuxPkgs;
    inherit crane fenix treefmt;
  };

  # MCP server registry (uses Linux packages for server binaries)
  mcpRegistry = import ../mcp {
    pkgs = linuxPkgs;
    rustProfile = imageProfilesModule.rust;
  };

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

    # Suppress the bypass-permissions acceptance dialog. Claude 2.1.x reads
    # this from userSettings; the legacy bypassPermissionsModeAccepted in
    # ~/.claude.json is migrated here at startup, but the entrypoint re-seeds
    # settings.json each container, so the flag must live in the seed too.
    skipDangerousModePermissionPrompt = true;

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
  #
  # `agent = "pi"` requires the caller to supply `piPkg` (a Linux-built
  # package whose `bin/` contains the `pi` binary); wrapix no longer ships
  # pi-mono in tree. Symmetric with `agent = "direct"` and `directRunner`.
  mkImage =
    {
      profile,
      entrypointSh,
      krunSupport ? false,
      claudePkg ? linuxPkgs.claude-code,
      claudeSettings ? baseClaudeSettings,
      mcpServerConfigs ? { },
      agent ? "claude",
      directRunner ? null,
      piPkg ? null,
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
        directRunner
        piPkg
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
      # (default) is a no-op; "pi" adds a consumer-supplied `piPkg`; "direct"
      # adds a consumer-supplied `directRunner` package. Both `pi` and
      # `direct` require their respective package argument — mkSandbox throws
      # "agent = \"pi\" requires piPkg" (resp. directRunner) when the package
      # is missing, since wrapix no longer ships pi-mono or loom in tree.
      agent ? "claude",
      # Linux-built package whose `bin/` directory contains the direct-runner
      # binary. Required when `agent == "direct"`. Consumers provide this
      # themselves (e.g. via `loom.packages.${system}.default`) since wrapix
      # no longer builds loom in-tree.
      directRunner ? null,
      # Linux-built package whose `bin/` directory contains the `pi` binary.
      # Required when `agent == "pi"`. Consumers provide this themselves
      # (e.g. via `loom.packages.${system}.pi-mono`); wrapix no longer ships
      # pi-mono in tree. When the caller asks for `agent = "pi"` without
      # supplying `piPkg`, the default raises a fail-fast
      # `throw "... piPkg ..."` so the misuse is caught at evaluation.
      piPkg ? if agent == "pi" then throw ''mkSandbox: agent = "pi" requires piPkg'' else null,
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

      # Profile-baked launcher (mounts, writableDirs, networkAllowlist) with
      # no image interpolation. The launcher reads its image at runtime from
      # WRAPIX_DEFAULT_IMAGE_REF / WRAPIX_DEFAULT_IMAGE_SOURCE (interactive
      # `wrapix run`) or from SpawnConfig (`wrapix spawn`).
      launcher =
        if isLinux then
          linuxSandbox.mkSandbox {
            profile = finalProfile;
            inherit
              cpus
              memoryMb
              deployKey
              networkAllowlist
              ;
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
          }
        else
          throw "Unsupported system: ${system}";

      # Expose the image derivation for consumers that manage containers
      # themselves (e.g. drive podman directly).
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
        inherit
          agent
          directRunner
          piPkg
          mcpServerConfigs
          ;
      };

      # Profile-specific sandbox: makeWrapper composes launcher + image,
      # baking in the agent-runtime selector and image ref/source as defaults
      # so `wrapix run` works without the caller exporting env vars.
      packageName = "wrapix-${finalProfile.name}${packageSuffix}";
      packageSuffix = if agent != "claude" then "-${agent}" else "";
      package =
        pkgs.runCommand packageName
          {
            nativeBuildInputs = [ pkgs.makeWrapper ];
            passthru = { inherit launcher image; };
            meta.mainProgram = "wrapix";
          }
          ''
            mkdir -p "$out/bin"
            makeWrapper "${launcher}/bin/wrapix" "$out/bin/wrapix" \
              --set WRAPIX_AGENT "${agent}" \
              --set-default WRAPIX_DEFAULT_IMAGE_REF "${mkImageRef image}" \
              --set-default WRAPIX_DEFAULT_IMAGE_SOURCE "${image}"
          '';

    in
    {
      inherit package image launcher;
      profile = finalProfile;
    };

in
{
  inherit
    mkSandbox
    mkImage
    mkImageRef
    profiles
    rustProfileFromFile
    baseClaudeSettings
    ;
  inherit (manifest) mkProfileImages;
}
