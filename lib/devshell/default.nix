{
  pkgs,
  rustCli,
  beads,
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

  hostNixConfig = pkgs.writeText "wrix-host-nix-config.sh" (readFile ../services/host-nix-config.sh);
  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };
  prekWrappers = import ../prek/wrappers.nix { inherit pkgs; };

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
            if git rev-parse --git-dir >/dev/null 2>&1 && [[ -f .pre-commit-config.yaml ]]; then
              _wrix_hooks_target='${hooksTarget}'
              if _wrix_hooks_current=$(git config --local --get core.hooksPath); then
                if [[ "$_wrix_hooks_current" != "$_wrix_hooks_target" ]]; then
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
          beads.shellHook
        else
          ''
            _wrix_service_bin="''${WRIX_BIN:-${rustCli.wrix}/bin/wrix}"
            if ! "$_wrix_service_bin" service start; then
              return 1 2>/dev/null || exit 1
            fi
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
      beadsHook = if nixCacheEnabled then beads.waitAndExport else "";
    in
    pkgs.mkShell {
      packages =
        (profile.hostPackages or profile.packages)
        ++ packages
        ++ [
          rustCli.wrix
          prekWrappers.prePushChecks
          prekWrappers.skipIfMissing
        ];
      env = profile.env // cacheEnv // env;
      shellHook = ''
        ${serviceHook}

        ${beadsHook}

        ${prekHookSetup}

        echo "Wrix development shell"

        ${profile.shellHook or ""}
        ${shellHook}
      '';
    };
}
