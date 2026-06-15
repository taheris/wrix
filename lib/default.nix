{
  pkgs,
  system,
  linuxPkgs ? pkgs,
  crane ? null,
  fenix ? null,
  treefmt ? null,
}:

let
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
  hostNixConfig = pkgs.writeText "wrix-host-nix-config.sh" (
    builtins.readFile ./services/host-nix-config.sh
  );

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
      nixCacheEnabled = if builtins.isAttrs nixCache then nixCache.enable or true else nixCache != false;
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
      env = profile.env // env;
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
