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
    attrNames
    attrValues
    concatMap
    elem
    filter
    isAttrs
    mapAttrs
    match
    seq
    ;
  inherit (pkgs.lib)
    concatStringsSep
    hasPrefix
    makeBinPath
    optionals
    ;

  isDarwin = elem system [ "aarch64-darwin" ];
  isLinux = elem system [
    "aarch64-linux"
    "x86_64-linux"
  ];
  krunRuntime = linuxPkgs.crun.overrideAttrs (old: {
    pname = "crun-krun";
    buildInputs = old.buildInputs ++ [ linuxPkgs.libkrun ];
    configureFlags = (old.configureFlags or [ ]) ++ [ "--with-libkrun" ];
    postFixup = (old.postFixup or "") + ''
      patchelf --add-rpath ${linuxPkgs.lib.getLib linuxPkgs.libkrun}/lib $out/bin/crun
    '';
  });

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

  knownCredentialNames = attrNames profiles.base.runtimeSecrets;
  runtimeSecretPolicies = [
    "optional"
    "required"
  ];
  bootstrapSensitiveEnvNames = [
    "BASHOPTS"
    "BASH_ENV"
    "ENV"
    "GLIBC_TUNABLES"
    "PATH"
    "PS4"
    "SHELLOPTS"
    "WRIX_FIREWALL_BACKEND"
    "WRIX_NETWORK"
    "WRIX_NOTIFY_TCP"
    "WRIX_WAIT_FOR_ROUTE"
  ];
  bootstrapSensitiveEnvPrefixes = [
    "BEADS_DOLT_SERVER_"
    "LD_"
    "WRIX_NETWORK_"
    "WRIX_NIX_CACHE_"
    "WRIX_PROJECT_CACHE_"
  ];
  isBootstrapSensitiveEnv =
    name:
    elem name bootstrapSensitiveEnvNames
    || builtins.any (prefix: hasPrefix prefix name) bootstrapSensitiveEnvPrefixes;

  validateRuntimeSecrets =
    runtimeSecrets:
    if !isAttrs runtimeSecrets then
      throw "runtimeSecrets must be an attribute set mapping environment names to 'optional' or 'required'"
    else
      let
        invalidNames = filter (name: match "^[A-Za-z_][A-Za-z0-9_]*$" name == null) (
          attrNames runtimeSecrets
        );
        invalidPolicies = filter (name: !elem runtimeSecrets.${name} runtimeSecretPolicies) (
          attrNames runtimeSecrets
        );
        bootstrapSensitiveNames = filter isBootstrapSensitiveEnv (attrNames runtimeSecrets);
      in
      if invalidNames != [ ] then
        throw "runtimeSecrets contains invalid environment names: ${concatStringsSep ", " invalidNames}"
      else if invalidPolicies != [ ] then
        throw "runtimeSecrets policies must be 'optional' or 'required': ${concatStringsSep ", " invalidPolicies}"
      else if bootstrapSensitiveNames != [ ] then
        throw "runtimeSecrets cannot declare sandbox bootstrap environment variables: ${concatStringsSep ", " bootstrapSensitiveNames}"
      else
        runtimeSecrets;

  validateStaticEnv =
    label: runtimeSecrets: staticEnv:
    let
      names = attrNames staticEnv;
      invalidNames = filter (name: match "^[A-Za-z_][A-Za-z0-9_]*$" name == null) names;
      forbidden = filter (
        name: elem name knownCredentialNames || builtins.hasAttr name runtimeSecrets
      ) names;
      bootstrapSensitiveNames = filter isBootstrapSensitiveEnv names;
    in
    if invalidNames != [ ] then
      throw "${label} contains invalid environment names: ${concatStringsSep ", " invalidNames}"
    else if forbidden != [ ] then
      throw "${label} cannot contain runtime credentials: ${concatStringsSep ", " forbidden}"
    else if bootstrapSensitiveNames != [ ] then
      throw "${label} cannot set sandbox bootstrap environment variables: ${concatStringsSep ", " bootstrapSensitiveNames}"
    else
      null;

  validateProfile =
    profile:
    let
      runtimeSecrets = validateRuntimeSecrets (profile.runtimeSecrets or { });
      staticEnvValidation = validateStaticEnv "profile.env" runtimeSecrets (profile.env or { });
      hostEnvValidation = validateStaticEnv "profile.hostEnv" runtimeSecrets (profile.hostEnv or { });
    in
    seq runtimeSecrets (
      seq staticEnvValidation (seq hostEnvValidation (profile // { inherit runtimeSecrets; }))
    );

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

  serviceImage = import ../services/image.nix {
    pkgs = linuxPkgs;
    hostPkgs = pkgs;
    inherit (imageRustCli) cacheServe;
    asTarball = isDarwin;
  };

  sandboxToolPackages = [ (linuxPkgs.lib.hiPrio imageRustCli.wrix) ];

  # Claude config (~/.claude.json) - onboarding state and runtime flags
  baseClaudeConfig = {
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
    defaultModel = "gpt-6-astra";
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
      networkBootstrapSh ? null,
      krunSupport ? false,
      claudeConfig ? baseClaudeConfig,
      claudeSettings ? baseClaudeSettings,
      piSettings ? basePiSettings,
      mcpServerConfigs ? { },
      mcpRuntime ? false,
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
        networkBootstrapSh
        krunSupport
        claudeConfig
        claudeSettings
        piSettings
        mcpServerConfigs
        mcpRuntime
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
      runtimeSecrets ? { },
      networkAllowlist ? [ ],
    }:
    validateProfile (
      profile
      // {
        packages = (profile.packages or [ ]) ++ packages;
        mounts = (profile.mounts or [ ]) ++ mounts;
        env = (profile.env or { }) // env;
        hostEnv = (profile.hostEnv or profile.env or { }) // env;
        runtimeSecrets = (profile.runtimeSecrets or { }) // runtimeSecrets;
        networkAllowlist = (profile.networkAllowlist or [ ]) ++ networkAllowlist;
      }
    );

  # Build MCP server configurations from the mcp attrset.
  # Every adapter consumes the same normalized stdio fields.
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
      mcpServers = mapAttrs (_name: s: {
        inherit (s.config) command;
        args = s.config.args or [ ];
        env = s.config.env or { };
      }) serverConfigs;
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
      runtimeSecrets ? { },
      mcp ? { },
      mcpRuntime ? false,
      agent ? "direct",
      agentPkg ? null,
      agentSettings ? { },
    }:
    let
      effectiveMcp = if mcpRuntime then mapAttrs (_: _: { }) mcpRegistry else mcp;
      mcpConfig = buildMcpConfig effectiveMcp;
      mcpServerConfigs = mcpConfig.mcpServers;
      finalProfile = extendProfile profile {
        packages = sandboxToolPackages ++ packages ++ mcpConfig.packages;
        inherit mounts env runtimeSecrets;
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
          validateStaticEnv "agentSettings.env" finalProfile.runtimeSecrets (agentSettings.env or { });

      finalAgentPkg = builtins.seq _validateAgentSettings (
        if agentPkg == null then defaultAgentPkg else agentPkg
      );

      finalClaudeConfig = baseClaudeConfig;

      claudeAgentSettings = if agent == "claude" then agentSettings else { };

      finalClaudeSettings =
        baseClaudeSettings
        // claudeAgentSettings
        // {
          env = baseClaudeSettings.env // (claudeAgentSettings.env or { });
        }
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
          krunRuntime
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
      serviceImageEnvSetup = ''
        export WRIX_SERVICE_IMAGE="''${WRIX_SERVICE_IMAGE:-${serviceImage.ref}}"
        export WRIX_SERVICE_IMAGE_SOURCE="''${WRIX_SERVICE_IMAGE_SOURCE:-${serviceImage.source}}"
        export WRIX_SERVICE_IMAGE_SOURCE_KIND="''${WRIX_SERVICE_IMAGE_SOURCE_KIND:-${serviceImage.source_kind}}"
        export WRIX_SERVICE_IMAGE_DIGEST="''${WRIX_SERVICE_IMAGE_DIGEST:-${serviceImage.digest}}"
      '';

      image = mkImage {
        profile = finalProfile;
        entrypointSh =
          if isLinux then
            ./linux/entrypoint.sh
          else if isDarwin then
            ./darwin/entrypoint.sh
          else
            null;
        networkBootstrapSh = if isDarwin then ./darwin/network-bootstrap.sh else null;
        krunSupport = isLinux;
        asTarball = isDarwin;
        claudeConfig = finalClaudeConfig;
        claudeSettings = finalClaudeSettings;
        piSettings = finalPiSettings;
        agentPkg = finalAgentPkg;
        inherit
          agent
          mcpRuntime
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
              optional = mount.optional or false;
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
            runtime_secrets = finalProfile.runtimeSecrets;
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
            ${serviceImageEnvSetup}
            exec ${launcher}/bin/wrix --profile-config ${profileConfig} "$@"
            WRIX_WRAPPER
            cat > "$out/bin/wrix-run" <<'WRIX_RUN_WRAPPER'
            #!${pkgs.runtimeShell}
            set -euo pipefail
            ${launcherRuntimePathSetup}
            ${serviceImageEnvSetup}
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
            ln -s "${launcher}/bin/wrix-prek" "$out/bin/wrix-prek"
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
    serviceImage
    baseClaudeSettings
    validateRuntimeSecrets
    ;
  inherit (manifest) mkProfileImages;
}
