{
  pkgs,
  system,
  linuxPkgs ? pkgs,
  crane ? null,
  fenix ? null,
  treefmt ? null,
}:

let
  inherit (builtins)
    concatStringsSep
    elemAt
    fromJSON
    hasAttr
    isAttrs
    isString
    match
    readFile
    ;

  sandbox = import ./sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      ;
  };
  beads = import ./beads { inherit pkgs linuxPkgs; };
  rustCli = import ./services/rust.nix {
    inherit pkgs;
    rustProfile = sandbox.profiles.rust;
  };
  tmuxMcp = import ./mcp/tmux/mcp-server.nix {
    inherit pkgs;
    rustProfile = sandbox.profiles.rust;
  };

  prekHooksBundle = import ./prek/bundle.nix { inherit pkgs; };
  prekWrappers = import ./prek/wrappers.nix { inherit pkgs; };
  hostNixConfig = pkgs.writeText "wrix-host-nix-config.sh" (readFile ./services/host-nix-config.sh);

  boolEnv = value: if value then "1" else "0";
  listEnv = values: concatStringsSep "\n" (map toString values);
  scaledValue =
    name: scales: value:
    let
      text = if isString value then value else toString value;
      parsed = match "([0-9]+)([A-Za-z]*)" text;
      suffix = elemAt parsed 1;
    in
    if parsed == null || !(hasAttr suffix scales) then
      throw "invalid nixCache.${name} value: ${text}"
    else
      toString ((fromJSON (elemAt parsed 0)) * scales.${suffix});
  sizeBytes = scaledValue "warnSize" {
    "" = 1;
    K = 1024;
    M = 1024 * 1024;
    G = 1024 * 1024 * 1024;
    T = 1024 * 1024 * 1024 * 1024;
  };
  durationSeconds =
    name:
    scaledValue name {
      "" = 1;
      s = 1;
      m = 60;
      h = 60 * 60;
      d = 24 * 60 * 60;
    };

in
{
  inherit (sandbox) profiles mkSandbox mkProfileImages;
  tmuxMcpPackage = tmuxMcp;
  inherit beads;
  rustPackage = rustCli;

  prekHooks = prekHooksBundle;
  inherit (prekWrappers) prePushChecks skipIfMissing;

  deriveProfile =
    baseProfile: extensions:
    baseProfile
    // extensions
    // {
      # corePackages is tier-1: extension grows packages only, never the
      # fixed-per-instance core.
      corePackages = baseProfile.corePackages or [ ];
      packages = (baseProfile.packages or [ ]) ++ (extensions.packages or [ ]);
      hostPackages = (baseProfile.hostPackages or [ ]) ++ (extensions.hostPackages or [ ]);
      mounts = (baseProfile.mounts or [ ]) ++ (extensions.mounts or [ ]);
      env = (baseProfile.env or { }) // (extensions.env or { });
      networkAllowlist = (baseProfile.networkAllowlist or [ ]) ++ (extensions.networkAllowlist or [ ]);
    };

  # Top-level constructor for project-pinned rust profiles. `toolchain` is the
  # path to a rust-toolchain.toml; `sha256` is the fenix purity hash. Both are
  # required — Nix's destructuring errors when either is omitted, matching the
  # "no silent unpinned profile" invariant in specs/profiles.md.
  rustProfile =
    {
      toolchain,
      sha256,
      packages ? [ ],
      env ? { },
      mounts ? [ ],
      networkAllowlist ? [ ],
    }:
    let
      base = sandbox.rustProfileFromFile {
        file = toolchain;
        inherit sha256;
      };
    in
    base
    // {
      packages = base.packages ++ packages;
      env = base.env // env;
      mounts = base.mounts ++ mounts;
      networkAllowlist = base.networkAllowlist ++ networkAllowlist;
    };

  mkDevShell =
    {
      profile,
      packages ? [ ],
      env ? { },
      shellHook ? "",
      prekHooks ? true,
      nixCache ? true,
    }:
    let
      hooksTarget =
        if prekHooks == false then
          null
        else if prekHooks == true then
          prekHooksBundle
        else
          prekHooks;
      prekHookSetup =
        if hooksTarget == null then
          ""
        else
          ''
            if [ -d .git ] && [ -f .pre-commit-config.yaml ]; then
              _wrix_hooks_target='${hooksTarget}'
              if _wrix_hooks_current=$(git config --local --get core.hooksPath); then
                if [ "$_wrix_hooks_current" != "$_wrix_hooks_target" ]; then
                  echo "wrix: overriding stale core.hooksPath ($_wrix_hooks_current) -> $_wrix_hooks_target" >&2
                fi
              fi
              git config --local core.hooksPath "$_wrix_hooks_target"
              unset _wrix_hooks_target _wrix_hooks_current
            fi
          '';
      defaultNixCache = {
        enable = true;
        requireTrustedNix = true;
        publish = {
          packages = true;
          checks = true;
          devShell = true;
          includeRoots = [ ];
          excludeRoots = [ ];
        };
        warm = {
          packages = true;
          checks = false;
          devShell = true;
          includeRoots = [ ];
          excludeRoots = [ ];
        };
        warnSize = "50G";
        pendingTtl = "7d";
        pruneInterval = "24h";
      };
      nixCacheConfig =
        if nixCache == false then
          defaultNixCache // { enable = false; }
        else if isAttrs nixCache then
          defaultNixCache
          // nixCache
          // {
            publish = defaultNixCache.publish // (nixCache.publish or { });
            warm = defaultNixCache.warm // (nixCache.warm or { });
          }
        else
          defaultNixCache;
      nixCacheEnabled = nixCacheConfig.enable;
      cacheEnv =
        if !nixCacheEnabled then
          { }
        else
          {
            WRIX_NIX_CACHE_REQUIRE_TRUSTED = boolEnv nixCacheConfig.requireTrustedNix;
            WRIX_CACHE_PUBLISH_PACKAGES = boolEnv nixCacheConfig.publish.packages;
            WRIX_CACHE_PUBLISH_CHECKS = boolEnv nixCacheConfig.publish.checks;
            WRIX_CACHE_PUBLISH_DEVSHELL = boolEnv nixCacheConfig.publish.devShell;
            WRIX_CACHE_PUBLISH_INCLUDE = listEnv nixCacheConfig.publish.includeRoots;
            WRIX_CACHE_PUBLISH_EXCLUDE = listEnv nixCacheConfig.publish.excludeRoots;
            WRIX_CACHE_WARM_PACKAGES = boolEnv nixCacheConfig.warm.packages;
            WRIX_CACHE_WARM_CHECKS = boolEnv nixCacheConfig.warm.checks;
            WRIX_CACHE_WARM_DEVSHELL = boolEnv nixCacheConfig.warm.devShell;
            WRIX_CACHE_WARM_INCLUDE = listEnv nixCacheConfig.warm.includeRoots;
            WRIX_CACHE_WARM_EXCLUDE = listEnv nixCacheConfig.warm.excludeRoots;
            WRIX_CACHE_SOFT_LIMIT_BYTES = sizeBytes nixCacheConfig.warnSize;
            WRIX_CACHE_PENDING_RETENTION_SECS = durationSeconds "pendingTtl" nixCacheConfig.pendingTtl;
            WRIX_CACHE_PRUNE_INTERVAL_SECS = durationSeconds "pruneInterval" nixCacheConfig.pruneInterval;
          };
      serviceHook =
        if !nixCacheEnabled then
          ''
            if [[ -d "$PWD/.beads/dolt" ]]; then
              _wrix_service_bin="''${WRIX_BIN:-${rustCli.wrix}/bin/wrix}"
              "$_wrix_service_bin" service start --no-cache
              unset _wrix_service_bin
            fi
          ''
        else
          ''
            _wrix_service_bin="''${WRIX_BIN:-${rustCli.wrix}/bin/wrix}"
            "$_wrix_service_bin" service start
            _wrix_nix_config=$(
              WRIX_SERVICE_BIN="$_wrix_service_bin" \
                WRIX_CACHE_HOOK_BIN="${rustCli.cacheHook}/bin/wrix-cache-hook" \
                WRIX_CACHE_PUBLISH_BIN="${rustCli.cachePublish}/bin/wrix-cache-publish" \
                WRIX_BASH_BIN="${pkgs.bash}/bin/bash" \
                WRIX_HOST_NIX_CONFIG_PRINT=1 \
                ${pkgs.bash}/bin/bash ${hostNixConfig}
            )
            export NIX_CONFIG="$_wrix_nix_config"
            unset _wrix_service_bin _wrix_nix_config
          '';
    in
    pkgs.mkShell {
      packages =
        (profile.hostPackages or profile.packages)
        ++ packages
        ++ [
          prekWrappers.prePushChecks
          prekWrappers.skipIfMissing
        ];
      env = profile.env // cacheEnv // env;
      shellHook = ''
        ${serviceHook}

        # Configure Dolt origin remote for bd dolt pull/push (no-op if already set)
        if [ -d .beads/dolt/beads/.dolt ] && [ -d .git/beads-worktrees/beads/.beads/dolt-remote ]; then
          _dolt_remote="file://$PWD/.git/beads-worktrees/beads/.beads/dolt-remote"
          (cd .beads/dolt/beads && dolt remote add origin "$_dolt_remote" 2>/dev/null || true)
        fi

        # Start per-workspace dolt container and export env vars suppressing
        # bd's embedded autostart. See lib/beads/default.nix.
        ${beads.shellHook}

        ${prekHookSetup}

        echo "Wrix development shell"

        ${profile.shellHook or ""}
        ${shellHook}
      '';
    };
}
