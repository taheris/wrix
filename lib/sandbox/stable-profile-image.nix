# wrapix-stable-profile-<name>: tier 1 of the provenance-tiered fromImage chain
# (specs/image-builder.md § Base Image Layering). Holds the fixed-per-instance
# closure — profile.corePackages plus the wrapix-generated derivations that do
# not vary with profile.packages, MCP configs, the merged Claude settings, or
# the agent runtime selection — chained atop wrapix-base-image. The per-profile
# leaf chains on top via fromImage, so this tier's tar loads into the platform
# store once and is not re-emitted when profile-level inputs change.
#
# Built with buildLayeredImage (a tar in the store) on every platform so it is
# usable as the fromImage of the leaf on both Linux and Darwin, matching the
# tar-only constraint base-image.nix records for tier 0 (the Linux
# streamLayeredImage Python shebang cannot execute on macOS).
{
  pkgs,
  profile,
}:

let
  inherit (pkgs) dockerTools;

  notifyClient = import ../notify/client.nix { inherit pkgs; };
  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };
  prekWrappers = import ../prek/wrappers.nix { inherit pkgs; };

  wrapixBaseImage = import ./base-image.nix { inherit pkgs; };
  baseContents = import ./base-contents.nix { inherit pkgs; };

  nixConfig = pkgs.writeTextDir "etc/nix/nix.conf" ''
    experimental-features = nix-command flakes
    sandbox = false
    filter-syscalls = false
    # The container runs Nix as rootless container-root (single identity, no
    # nix-daemon). An empty build-users-group makes Nix build directly as that
    # process instead of demanding the default 'nixbld' group (absent here) to
    # drop privileges — which it only does when euid is really 0. Without this,
    # `nix build`/`nix develop` as root fail: "the group 'nixbld' specified in
    # 'build-users-group' does not exist" (wx-nsage).
    build-users-group =
  '';

  passwdFile = pkgs.writeTextDir "etc/passwd" ''
    root:x:0:0:root:/root:/bin/bash
    nobody:x:65534:65534:Unprivileged account:/var/empty:/bin/false
    wrapix:x:1000:1000:Wrapix Sandbox:/home/wrapix:/bin/bash
  '';

  groupFile = pkgs.writeTextDir "etc/group" ''
    root:x:0:
    nogroup:x:65534:
    wrapix:x:1000:
  '';

  # The packages the stable-profile buildEnv links. Factored out because the
  # baked Nix DB must register these (which buildLayeredImage materializes),
  # NOT the `coreEnv` wrapper itself (which it does not — see lowerTiersContents).
  # No agent binary lives here — claude-code rides the agent tier above.
  coreEnvPaths = [
    notifyClient
    prekWrappers.prePushChecks
    prekWrappers.skipIfMissing
  ]
  ++ (profile.corePackages or [ ]);

  coreEnv = pkgs.buildEnv {
    name = "wrapix-stable-profile-env";
    paths = coreEnvPaths;
    pathsToLink = [
      "/bin"
      "/share"
      "/etc"
      "/lib"
    ];
  };

  tierContents = [
    passwdFile
    groupFile
    nixConfig
    coreEnv
  ];

  # The on-disk roots tiers 0+1 contribute to the composed image: the base
  # contents, this tier's own contents, and prekHooksBundle (reachable only via
  # config.Env). Exposed so the leaf can remove_paths the whole union from its
  # layering graph (specs/image-builder.md § Base Image Layering).
  lowerTiersRootPaths = baseContents ++ tierContents ++ [ prekHooksBundle ];

  # registers the baked Nix DB over the materialized contents closure, not the
  # build derivation's full closure (specs/image-builder.md § In-Container Nix
  # Store Consistency). coreEnv (a buildEnv) is replaced by its `paths`:
  # dockerTools copies a buildEnv's merged tree to the image root and
  # materializes only its targets, never the wrapper's own store path — so
  # registering the wrapper bakes a dangling path. prekHooksBundle is included:
  # it rides in `config.Env` (WRAPIX_PREK_HOOKS) and its closure IS materialized
  # into a layer, so omitting it leaves an orphan (on-disk-but-unregistered).
  lowerTiersContents =
    baseContents
    ++ [
      passwdFile
      groupFile
      nixConfig
      prekHooksBundle
    ]
    ++ coreEnvPaths;

  # Everything tiers 0+1 ship, exposed so the leaf's custom layeringPipeline can
  # remove_paths the whole union (specs/image-builder.md § Base Image Layering).
  # The custom pipeline does not dedup fromImage, so the leaf must strip this
  # explicitly.
  lowerTiersClosure = pkgs.closureInfo {
    rootPaths = lowerTiersRootPaths;
  };

  # Bounded to this tier's own fixed closure; the 127-layer OCI ceiling splits
  # across base (64) + stable-profile (40) + agent (8) + leaf (15).
  maxLayers = 40;

  # Split only this tier's own paths into layers. dockerMakeLayers (the custom
  # layeringPipeline path) does not dedup fromImage the way the default
  # popularity-contest path does, so remove_paths strips the lower tier's
  # (wrapix-base-image's) closure first — a path base already ships is never
  # re-emitted here.
  layeringPipeline =
    pkgs.runCommandLocal "wrapix-stable-profile-${profile.name}-layering.json"
      {
        nativeBuildInputs = [ pkgs.jq ];
        baseClosure = pkgs.closureInfo { rootPaths = baseContents; };
      }
      ''
        set -euo pipefail
        jq -n \
          --rawfile storePaths "$baseClosure/store-paths" \
          --argjson maxLayers ${toString maxLayers} \
          '($storePaths | split("\n") | map(select(length > 0))) as $base
           | [
               [ "remove_paths", $base ],
               [ "popularity_contest" ],
               [ "limit_layers", $maxLayers ]
             ]' \
          > "$out"
      '';
in
dockerTools.buildLayeredImage {
  name = "wrapix-stable-profile-${profile.name}";
  tag = "latest";
  fromImage = wrapixBaseImage;
  inherit layeringPipeline;

  contents = tierContents;

  # Pin prekHooksBundle into this tier's closure without symlinking its hooks
  # into the image root — the entrypoint reaches it by store path via this env
  # var (specs/pre-commit.md § Bead-Container Hook Installation).
  config.Env = [
    "WRAPIX_PREK_HOOKS=${prekHooksBundle}"
  ];
}
// {
  inherit lowerTiersClosure lowerTiersRootPaths lowerTiersContents;
}
