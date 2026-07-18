{
  pkgs,
  system,
  linuxPkgs,
  crane,
  fenix,
  treefmt,
  serviceCli,
}:

let
  inherit (builtins)
    concatMap
    elem
    mapAttrs
    attrValues
    ;
  inherit (pkgs.lib) makeBinPath optionals;

  isDarwin = elem system [
    "aarch64-darwin"
    "x86_64-darwin"
  ];
  isLinux = elem system [
    "aarch64-linux"
    "x86_64-linux"
  ];

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
  # `wrix.rustProfile` (lib/default.nix); it is intentionally stripped from
  # the public `profiles` surface — consumers reach pinned rust profiles
  # through `wrix.rustProfile { toolchain; sha256; }`.
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

  imageRustCli = import ../services/rust.nix {
    pkgs = linuxPkgs;
    rustProfile = imageProfilesModule.rust;
  };

  sandboxToolPackages = [ (linuxPkgs.lib.hiPrio imageRustCli.wrix) ];

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
      ANTHROPIC_MODEL = "claude-opus-4-8";
      CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1";
      CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS = "1";
      DISABLE_AUTOUPDATER = "1";
      DISABLE_ERROR_REPORTING = "1";
      DISABLE_TELEMETRY = "1";
    };

    hooks = {
      Stop = [
        {
          matcher = "";
          hooks = [
            {
              type = "command";
              command = "wrix-notify 'Claude Code' 'Waiting for input...'";
            }
          ];
        }
      ];
    };
  };

  # Pi settings (~/.pi/agent/settings.json) - non-secret provider preferences.
  # Credentials stay runtime-only in ~/.pi/agent/auth.json (mounted by the
  # launcher when WRIX_AGENT=pi).
  basePiSettings = {
    defaultProvider = "openai-codex";
    defaultModel = "gpt-5.6-sol";
    defaultThinkingLevel = "xhigh";
    defaultProjectTrust = "always";
    editorPaddingX = 1;
    enableInstallTelemetry = false;
    steeringMode = "all";
    followUpMode = "all";
    sessionDir = "/workspace/.pi/agent/sessions";
    transport = "websocket-cached";
  };

  defaultDirectRunner = linuxPkgs.writeShellApplication {
    name = "loom-direct-runner";
    text = ''
      echo "wrix: default direct runner is a placeholder; provide agentPkg for agent=direct" >&2
      exit 64
    '';
  };

  # Build the container image using Linux packages
  # On Darwin, this will use a remote Linux builder if configured
  #
  # `agent = "pi"` defaults to nixpkgs' pi-coding-agent (a Linux-built package
  # whose `bin/` contains the `pi` binary). Symmetric with `agent = "direct"`
  # and `agentPkg`, both remain overrideable.
  mkImage =
    {
      profile,
      entrypointSh,
      krunSupport ? false,
      claudeSettings ? baseClaudeSettings,
      piSettings ? basePiSettings,
      mcpServerConfigs ? { },
      agent,
      agentPkg,
      asTarball ? false,
    }:
    import ./image.nix {
      pkgs = linuxPkgs;
      hostPkgs = pkgs;
      inherit
        profile
        entrypointSh
        krunSupport
        claudeConfig
        claudeSettings
        piSettings
        mcpServerConfigs
        agent
        agentPkg
        asTarball
        ;
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
      # Agent runtime axis composed onto the workspace profile. "direct" is
      # the default base image; "claude" and "pi" are explicit agent overlays.
      agent ? "direct",
      # Linux-built package whose `bin/` directory contains the selected
      # agent binary. Defaults according to `agent`.
      agentPkg ? null,
      # Settings for the selected agent. Schema depends on `agent`.
      agentSettings ? { },
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

      # Extend profile with wrix-owned sandbox tools, user packages, and MCP server packages.
      finalProfile = extendProfile profile {
        packages = sandboxToolPackages ++ packages ++ mcpConfig.packages;
        inherit mounts env;
      };

      defaultAgentPkg =
        {
          direct = defaultDirectRunner;
          claude = linuxPkgs.claude-code;
          pi = linuxPkgs.pi-coding-agent;
        }
        .${agent} or (throw "mkSandbox: unknown agent '${agent}' (expected 'direct', 'claude', or 'pi')");

      _validateAgentSettings =
        if agent == "direct" && agentSettings != { } then
          throw "mkSandbox: agentSettings is only supported for agent='claude' or agent='pi'"
        else
          null;

      finalAgentPkg = builtins.seq _validateAgentSettings (
        if agentPkg == null then defaultAgentPkg else agentPkg
      );

      # Merge MCP servers and profile plugins into Claude settings. When
      # mcpRuntime is true, don't bake mcpServers — entrypoint handles it.
      claudeAgentSettings = if agent == "claude" then agentSettings else { };

      finalClaudeSettings =
        baseClaudeSettings
        // claudeAgentSettings
        // {
          env = baseClaudeSettings.env // (claudeAgentSettings.env or { });
        }
        // (if !mcpRuntime && mcpConfig.mcpServers != { } then { inherit (mcpConfig) mcpServers; } else { })
        // (
          if (finalProfile.enabledPlugins or { }) != { } then
            { inherit (finalProfile) enabledPlugins; }
          else
            { }
        );

      finalPiSettings = basePiSettings // (if agent == "pi" then agentSettings else { });

      launcher = if isLinux || isDarwin then serviceCli else throw "Unsupported system: ${system}";
      launcherRuntimePath = makeBinPath (
        [ pkgs.nix ]
        ++ optionals isLinux [
          pkgs.podman
          pkgs.skopeo
        ]
        ++ optionals isDarwin [ pkgs.skopeo ]
      );
      launcherRuntimePathSetup = ''
        if [[ -n "''${PATH:-}" ]]; then
          export PATH="${launcherRuntimePath}:$PATH"
        else
          export PATH="${launcherRuntimePath}"
        fi
      '';

      # Expose the image derivation for consumers that inspect the image directly.
      # Its `.source` metadata is the platform install source: a Linux descriptor
      # or a Darwin tar-loadable archive.
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
        piSettings = finalPiSettings;
        agentPkg = finalAgentPkg;
        inherit
          agent
          mcpServerConfigs
          ;
      };

      profileConfigBase = pkgs.writeText "${packageName}-profile-config-base.json" (
        builtins.toJSON {
          schema = 1;
          inherit system;
          profile = {
            inherit (finalProfile) name;
            env = finalProfile.env or { };
            mounts = map (mount: {
              inherit (mount) source dest;
              mode = mount.mode or "ro";
            }) (finalProfile.mounts or [ ]);
            writable_dirs = finalProfile.writableDirs or [ ];
            network_allowlist = finalProfile.networkAllowlist or [ ];
          };
          image = {
            ref = mkImageRef image;
            source = "${image.source}";
            inherit (image) source_kind;
            digest = "";
          };
          agent = {
            kind = agent;
          };
          resources = {
            inherit cpus;
            memory_mb = memoryMb;
            pids_limit = 4096;
          };
          security = {
            deploy_key = deployKey;
          };
          network = {
            default_mode = "open";
            ipv6 = "disabled";
          };
          services = {
            beads = {
              enable = "auto";
            };
            nix_cache = {
              enable = true;
            };
          };
          features = {
            mcp_runtime = mcpRuntime;
          };
        }
      );

      profileConfig =
        pkgs.runCommand "${packageName}-profile-config.json" { nativeBuildInputs = [ pkgs.jq ]; }
          ''
            set -euo pipefail
            jq --arg digest "$(cat ${image.digest})" '.image.digest = $digest' ${profileConfigBase} > "$out"
          '';

      imageWithConfig = image // {
        inherit agent profileConfig;
      };

      # Profile-specific sandbox: wrapper composes launcher + immutable
      # ProfileConfig so `wrix run` works without mutable image or agent env.
      packageName = "wrix-${finalProfile.name}${packageSuffix}";
      packageSuffix = if agent == "direct" then "" else "-${agent}";
      package =
        pkgs.runCommand packageName
          {
            passthru = {
              image = imageWithConfig;
              inherit launcher profileConfig;
            };
            meta.mainProgram = "wrix-run";
          }
          ''
            mkdir -p "$out/bin"
            cat > "$out/bin/wrix" <<'WRIX_WRAPPER'
            #!${pkgs.runtimeShell}
            set -euo pipefail
            ${launcherRuntimePathSetup}
            exec ${launcher}/bin/wrix --profile-config ${profileConfig} "$@"
            WRIX_WRAPPER
            cat > "$out/bin/wrix-run" <<'WRIX_RUN_WRAPPER'
            #!${pkgs.runtimeShell}
            set -euo pipefail
            ${launcherRuntimePathSetup}
            case "''${1:-}" in
              run|spawn|service|beads)
                exec ${launcher}/bin/wrix --profile-config ${profileConfig} "$@"
                ;;
              *)
                exec ${launcher}/bin/wrix --profile-config ${profileConfig} run "$@"
                ;;
            esac
            WRIX_RUN_WRAPPER
            cat > "$out/bin/wrix-git-sign" <<'WRIX_GIT_SIGN_WRAPPER'
            #!${pkgs.runtimeShell}
            set -euo pipefail
            exec ${launcher}/bin/wrix-git-sign "$@"
            WRIX_GIT_SIGN_WRAPPER
            chmod +x "$out/bin/wrix"
            chmod +x "$out/bin/wrix-run"
            chmod +x "$out/bin/wrix-git-sign"
          '';

    in
    {
      inherit package launcher profileConfig;
      image = imageWithConfig;
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
