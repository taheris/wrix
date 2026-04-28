# NixOS module for services.wrapix.cities.<name>
#
# Generates systemd units, a podman network per city, and invokes mkCity
# to produce city.toml and container images.
#
# See specs/gas-city.md for the full specification.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    filterAttrs
    mapAttrs
    mapAttrs'
    mapAttrsToList
    mkIf
    mkMerge
    mkOption
    nameValuePair
    types
    ;

  cfg = config.services.wrapix;

  # Import wrapix library — the module receives pkgs with overlays applied
  wrapix = import ../lib {
    inherit pkgs;
    inherit (pkgs.stdenv.hostPlatform) system;
    linuxPkgs = pkgs;
  };

  shellLib = import ../lib/util/shell.nix { };

  # Resolve a profile string shorthand to a profile attrset
  resolveProfile =
    p:
    if builtins.isString p then
      wrapix.profiles.${p}
        or (throw "services.wrapix.cities: unknown profile '${p}', expected one of: ${builtins.concatStringsSep ", " (builtins.attrNames wrapix.profiles)}")
    else
      p;

  # Per-city submodule options
  cityOpts =
    { name, ... }:
    {
      options = {
        enable = mkOption {
          type = types.bool;
          default = true;
          description = "Whether to enable wrapix city '${name}'.";
        };

        workspace = mkOption {
          type = types.path;
          description = "Workspace directory (required on NixOS — no flake root).";
        };

        profile = mkOption {
          type = types.either types.str (types.attrsOf types.anything);
          default = "base";
          description = ''
            Profile for agent containers. String shorthand (e.g. "rust", "python",
            "base") is resolved via wrapix.profiles. An attrset is passed through
            directly.
          '';
        };

        services = mkOption {
          type = types.attrsOf (
            types.submodule {
              options = {
                package = mkOption {
                  type = types.package;
                  description = "Nix package for this service.";
                };
                cmd = mkOption {
                  type = types.listOf types.str;
                  default = [ ];
                  description = "Override entrypoint command. Defaults to package binary.";
                };
                environment = mkOption {
                  type = types.attrsOf types.str;
                  default = { };
                  description = "Environment variables for the service container.";
                };
                ports = mkOption {
                  type = types.listOf types.port;
                  default = [ ];
                  description = "Exposed TCP ports.";
                };
                volumes = mkOption {
                  type = types.listOf types.str;
                  default = [ ];
                  description = "Volume mounts (host:container format).";
                };
              };
            }
          );
          default = { };
          description = "Service containers managed by Gas City.";
        };

        secrets = mkOption {
          type = types.attrsOf types.str;
          default = { };
          description = ''
            Secrets mapping. String starting with "/" = file path (works with
            sops-nix, agenix, etc.). Any other string = host environment variable
            name. The "claude" secret is required when services are defined.
          '';
          example = {
            claude = "/run/secrets/claude-api-key";
            deployKey = "/run/secrets/deploy-key";
          };
        };

        agent = mkOption {
          type = types.str;
          default = "claude";
          description = "Agent type. Only 'claude' is supported.";
        };

        workers = mkOption {
          type = types.ints.positive;
          default = 1;
          description = "Maximum concurrent workers.";
        };

        cooldown = mkOption {
          type = types.str;
          default = "0";
          description = ''
            Time between task dispatches. Supports "30m", "1h", "2h30m", etc.
          '';
        };

        scout = mkOption {
          type = types.submodule {
            options = {
              interval = mkOption {
                type = types.str;
                default = "5m";
                description = "Scout polling interval.";
              };
              maxBeads = mkOption {
                type = types.ints.positive;
                default = 10;
                description = "Maximum open beads before scout pauses.";
              };
            };
          };
          default = { };
          description = "Scout configuration.";
        };

        resources = mkOption {
          type = types.attrsOf (
            types.submodule {
              options = {
                cpus = mkOption {
                  type = types.nullOr types.ints.positive;
                  default = null;
                  description = "CPU limit for this role.";
                };
                memory = mkOption {
                  type = types.nullOr types.str;
                  default = null;
                  description = "Memory limit for this role (e.g. \"4g\").";
                };
              };
            }
          );
          default = { };
          description = "Per-role resource limits (worker, scout, reviewer).";
          example = {
            worker = {
              cpus = 2;
              memory = "4g";
            };
          };
        };
      };
    };

  # Build the city derivation for a given city config
  mkCityForConfig =
    cityName: cityCfg:
    let
      profile = resolveProfile cityCfg.profile;

      # Convert service options to mkCity format
      serviceAttrs = mapAttrs (
        _svcName: svc:
        {
          inherit (svc) package;
        }
        // (if svc.cmd != [ ] then { inherit (svc) cmd; } else { })
        // (if svc.environment != { } then { inherit (svc) environment; } else { })
        // (if svc.ports != [ ] then { inherit (svc) ports; } else { })
      ) cityCfg.services;

      # Convert resource options — filter out null values
      resourceAttrs = mapAttrs (
        _role: res:
        filterAttrs (_: v: v != null) {
          inherit (res) cpus memory;
        }
      ) (filterAttrs (_: res: res.cpus != null || res.memory != null) cityCfg.resources);
    in
    wrapix.mkCity {
      name = cityName;
      services = serviceAttrs;
      inherit profile;
      inherit (cityCfg)
        agent
        workers
        cooldown
        secrets
        ;
      scout = {
        inherit (cityCfg.scout) interval maxBeads;
      };
      resources = resourceAttrs;
    };

  # Build secret flags for podman run — returns a list of shell-escaped args
  # All enabled cities
  enabledCities = filterAttrs (_: cityCfg: cityCfg.enable) cfg.cities;

  # Host-side tools the daemon shells out to (entrypoint.sh + provider.sh + gc)
  daemonTools = with pkgs; [
    bash
    beads
    beads-dolt
    beads-push
    coreutils
    dolt
    findutils
    gc
    gnugrep
    gnused
    jq
    podman
    util-linux
  ];

  # Systemd service units — runs entrypoint.sh on the host (not inside a container).
  # Agent role containers are spawned as siblings by provider.sh via local podman.
  cityServices = mapAttrs' (
    name: cityCfg:
    let
      city = mkCityForConfig name cityCfg;

      loadImages = pkgs.writeShellScript "load-images-${name}" (
        ''
          set -euo pipefail
          if ! ${pkgs.podman}/bin/podman image exists "${city.imageName}" 2>/dev/null; then
            ${city.sandbox.image} | ${pkgs.podman}/bin/podman load
            ${pkgs.podman}/bin/podman tag "localhost/wrapix-${city.sandbox.profile.name}:latest" "${city.imageName}" 2>/dev/null || true
          fi
          # Prune runs unconditionally — stale tags from other profiles don't
          # show up in this city's load path, so gate-free pruning is the
          # only way to sweep them.
          ${shellLib.pruneStaleImages { cmd = "${pkgs.podman}/bin/podman"; }}
        ''
        + builtins.concatStringsSep "" (
          mapAttrsToList (svcName: _svc: ''
            ${city.serviceImages.${svcName}} | ${pkgs.podman}/bin/podman load
          '') cityCfg.services
        )
      );

      startScript = pkgs.writeShellScript "start-city-${name}" ''
        set -euo pipefail
        export GC_CITY_NAME="${name}"
        export GC_WORKSPACE="${toString cityCfg.workspace}"
        export GC_AGENT_IMAGE="${city.imageName}"
        export GC_PODMAN_NETWORK="${city.networkName}"
        export GC_SECRET_FLAGS=${lib.escapeShellArg city.secretFlags}

        cd "$GC_WORKSPACE"

        # Stage city-config from the store
        mkdir -p .wrapix/city
        _city_stage_tmp=".wrapix/city/.staging.$$"
        rm -rf "$_city_stage_tmp"
        cp -rL --no-preserve=mode ${city.configDir} "$_city_stage_tmp"
        chmod -R u+w "$_city_stage_tmp"
        rm -rf .wrapix/city/current
        mv -T "$_city_stage_tmp" .wrapix/city/current
        cp -f --remove-destination .wrapix/city/current/city.toml city.toml

        mkdir -p .gc/formulas .gc/scripts .gc/cache .gc/system .gc/runtime
        touch .gc/events.jsonl
        for f in ${city.formulas}/*.formula.toml; do
          cp -f --remove-destination "$f" .gc/formulas/
        done
        chmod -R u+w .gc/formulas/orders 2>/dev/null || true
        rm -rf .gc/formulas/orders
        cp -r --no-preserve=mode ${city.formulas}/orders .gc/formulas/
        for f in ${city.scripts}/*; do
          ln -sf "$f" .gc/scripts/"$(basename "$f")"
        done

        exec .gc/scripts/entrypoint.sh
      '';
    in
    nameValuePair "wrapix-city-${name}" {
      description = "Wrapix Gas City: ${name}";
      after = [
        "network-online.target"
        "podman.service"
        "wrapix-city-${name}-network.service"
      ];
      requires = [
        "wrapix-city-${name}-network.service"
      ];
      wantedBy = [ "multi-user.target" ];

      path = daemonTools;

      serviceConfig = {
        Type = "exec";
        Restart = "always";
        RestartSec = 10;
        ExecStartPre = [ "${loadImages}" ];
        ExecStart = "${startScript}";
      };
    }
  ) enabledCities;

  # Systemd oneshot units for podman network creation
  networkServices = mapAttrs' (
    name: _cityCfg:
    let
      networkName = "city-${name}";
    in
    nameValuePair "wrapix-city-${name}-network" {
      description = "Podman network for wrapix city: ${name}";
      after = [ "podman.service" ];
      requires = [ "podman.service" ];
      wantedBy = [ "multi-user.target" ];

      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = pkgs.writeShellScript "create-network-${name}" ''
          ${pkgs.podman}/bin/podman network create ${networkName} || true
        '';
        ExecStop = pkgs.writeShellScript "remove-network-${name}" ''
          ${pkgs.podman}/bin/podman network rm ${networkName} || true
        '';
      };
    }
  ) enabledCities;

in
{
  options.services.wrapix = {
    cities = mkOption {
      type = types.attrsOf (types.submodule cityOpts);
      default = { };
      description = "Gas City instances managed by wrapix.";
    };
  };

  config = mkIf (enabledCities != { }) {
    virtualisation.podman.enable = true;

    systemd.services = mkMerge [
      cityServices
      networkServices
    ];
  };
}
