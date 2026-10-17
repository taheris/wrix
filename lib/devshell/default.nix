{
  pkgs,
  rustCli,
  beads,
  serviceImage,
}:

let
  inherit (builtins)
    attrNames
    concatStringsSep
    elemAt
    fromJSON
    hasAttr
    isAttrs
    isString
    isInt
    match
    readFile
    ;

  inherit (pkgs.lib)
    escapeShellArg
    escapeShellArgs
    optionalAttrs
    toShellVars
    ;

  hostNixConfig = pkgs.writeText "wrix-host-nix-config.sh" (readFile ../services/host-nix-config.sh);
  startIndependent = import ../services/start.nix { inherit (pkgs.stdenv.hostPlatform) isDarwin; };
  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };
  prekRunner = import ../prek/runner.nix { inherit pkgs; };
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

in
{
  mkDevShell =
    {
      sandbox ? null,
      profile ? null,
      packages ? [ ],
      env ? { },
      shellHook ? "",
      prekHooks ? true,
      nixCache ? true,
    }:
    let
      resolvedProfile =
        if sandbox != null && profile != null then
          throw "mkDevShell accepts either `sandbox` or `profile`, not both"
        else if sandbox != null then
          sandbox.profile
        else if profile != null then
          profile
        else
          throw "mkDevShell requires either `profile` or `sandbox`";
      wrixPackage = if sandbox == null then rustCli.wrix else sandbox.package;
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
              ${prekRunner}/bin/wrix-prek --bind || { return 1 2>/dev/null || exit 1; }
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
      nixCacheOverrides =
        if nixCache == false then
          { enable = false; }
        else if isAttrs nixCache then
          nixCache
        else if nixCache == true then
          { }
        else
          throw "nixCache must be a boolean or attrset";
      nixCacheEnabled =
        if hasAttr "pruneInterval" nixCacheOverrides then
          throw "nixCache.pruneInterval is no longer supported; collection is host-managed"
        else
          nixCacheOverrides.enable or true;
      overrideEnv =
        config: field: name: convert:
        optionalAttrs (hasAttr field config) { ${name} = convert config.${field}; };
      rootsEnv =
        prefix: config:
        overrideEnv config "packages" "${prefix}_PACKAGES" boolEnv
        // overrideEnv config "checks" "${prefix}_CHECKS" boolEnv
        // overrideEnv config "devShell" "${prefix}_DEVSHELL" boolEnv
        // overrideEnv config "includeRoots" "${prefix}_INCLUDE" listEnv
        // overrideEnv config "excludeRoots" "${prefix}_EXCLUDE" listEnv;
      cacheEnv =
        overrideEnv nixCacheOverrides "enable" "WRIX_NIX_CACHE_ENABLE" boolEnv
        // overrideEnv nixCacheOverrides "requireTrustedNix" "WRIX_NIX_CACHE_REQUIRE_TRUSTED" boolEnv
        // rootsEnv "WRIX_CACHE_PUBLISH" (nixCacheOverrides.publish or { })
        // rootsEnv "WRIX_CACHE_WARM" (nixCacheOverrides.warm or { })
        // overrideEnv nixCacheOverrides "warnSize" "WRIX_CACHE_SOFT_LIMIT_BYTES" sizeBytes
        // optionalAttrs (hasAttr "pendingTtl" nixCacheOverrides && isInt nixCacheOverrides.pendingTtl) {
          WRIX_CACHE_PENDING_RETENTION_SECS = toString nixCacheOverrides.pendingTtl;
        };
      durationHook =
        if
          !(hasAttr "pendingTtl" nixCacheOverrides)
          || isInt nixCacheOverrides.pendingTtl
          || hasAttr "WRIX_CACHE_PENDING_RETENTION_SECS" env
        then
          ""
        else
          ''
            WRIX_CACHE_PENDING_RETENTION_SECS=$(
              ${rustCli.cachePublish}/bin/wrix-cache-publish --duration-seconds ${escapeShellArg nixCacheOverrides.pendingTtl}
            ) || { return 1 2>/dev/null || exit 1; }
            export WRIX_CACHE_PENDING_RETENTION_SECS
          '';
      serviceEnv = {
        WRIX_SERVICE_IMAGE = serviceImage.ref;
        WRIX_SERVICE_IMAGE_SOURCE = "${serviceImage.source}";
        WRIX_SERVICE_IMAGE_SOURCE_KIND = serviceImage.source_kind;
        WRIX_SERVICE_IMAGE_DIGEST = "${serviceImage.digest}";
      };
      serviceHook =
        if !nixCacheEnabled then
          beads.shellHook
        else
          ''
            _wrix_service_bin="''${WRIX_BIN:-${wrixPackage}/bin/wrix}"
            ${startIndependent {
              command = ''"$_wrix_service_bin" service start'';
              onFailure = "return 1 2>/dev/null || exit 1";
            }}
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
      consumerEnvHook =
        if env == { } then
          ""
        else
          ''
            ${toShellVars env}
            export ${escapeShellArgs (attrNames env)}
          '';
    in
    pkgs.mkShell {
      packages =
        (resolvedProfile.hostPackages or resolvedProfile.packages)
        ++ packages
        ++ [
          wrixPackage
          prekWrappers.prePushChecks
          prekWrappers.skipIfMissing
        ];
      env = (resolvedProfile.hostEnv or resolvedProfile.env) // serviceEnv // cacheEnv // env;
      shellHook = ''
        ${durationHook}
        ${serviceHook}

        ${beadsHook}

        ${prekHookSetup}

        echo "Wrix development shell"

        ${resolvedProfile.shellHook or ""}
        ${consumerEnvHook}
        ${shellHook}
      '';
    };
}
