# Build the main OCI image for wrix sandbox
#
# This creates a layered container image with:
# - Base packages + profile-specific packages
# - Agent package selected by the caller
# - Optional consumer-supplied `agentPkg`
# - CA certificates for HTTPS
# - Platform-specific entrypoint script
#
# The image is composed from two orthogonal axes:
#   - workspace profile (base | rust | python) — toolchain packages
#   - agent runtime (claude | pi | direct) — agent binary layer
#
# The claude runtime layer adds `claude-code`; the pi runtime layer adds
# `pi-coding-agent`; the direct runtime layer adds the selected direct runner.
# Callers override any selected runtime with `agentPkg`.
#
# Layer ordering: stable packages first, frequently-changing packages last.
# This maximizes layer cache hits across rebuilds and profiles.
{
  pkgs,
  hostPkgs ? pkgs,
  profile,
  entrypointSh,
  networkBootstrapSh ? null,
  krunSupport ? false,
  claudeConfig,
  claudeSettings,
  piSettings ? { },
  mcpServerConfigs ? { },
  # Agent runtime axis. Callers must choose explicitly. "claude" adds
  # claude-code; "pi" adds pi-coding-agent; "direct" adds the direct-runner
  # binary. The resolved package is supplied as `agentPkg`.
  agent,
  # Linux-built package whose `bin/` directory contains the selected agent
  # binary (`claude`, `pi`, or `loom-direct-runner`).
  agentPkg,
  # Use buildLayeredImage (tar in store) instead of streamLayeredImage (script).
  # Required on Darwin where the stream script's Linux Python shebang won't execute.
  asTarball ? false,
}:

let
  inherit (pkgs.lib)
    concatStringsSep
    mapAttrsToList
    optionalString
    ;
  sshConfig = import ../util/ssh.nix;
  knownHosts = import ./known-hosts.nix { inherit pkgs; };

  # Darwin assembles Linux-targeted archives natively to avoid QEMU.
  imageBuilderPkgs = if asTarball then hostPkgs else pkgs;

  notifyClient = import ../notify/client.nix { inherit pkgs; };

  # Shared nixpkgs-pin-dependent bottom-of-closure. Chained under the
  # per-profile image via `fromImage` so it loads into the platform store once
  # (specs/image-builder.md § Base Image Layering).
  wrixBaseImage = import ./base-image.nix { inherit pkgs imageBuilderPkgs; };

  # Tier 1: the fixed-per-instance closure (profile.corePackages + the
  # wrix-generated derivations that do not vary with profile.packages, MCP
  # configs, Claude settings, or agent selection), chained atop the base. No
  # agent binary lives here — it rides the agent tier above (specs/image-builder.md
  # § Provenance-Tiered Layering).
  stableProfileImage = import ./stable-profile-image.nix {
    inherit pkgs imageBuilderPkgs profile;
  };

  # Bundle referenced from config.Env WRIX_PREK_HOOKS so the entrypoint can
  # point `core.hooksPath` at it (specs/pre-commit.md § Bead-Container Hook
  # Installation).
  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };

  # `pre-push-checks` and `skip-if-missing` wrappers — co-located on PATH so
  # `.pre-commit-config.yaml` entries that name them resolve inside the
  # container (specs/pre-commit.md § Hook-Entry Wrappers).
  prekWrappers = import ../prek/wrappers.nix { inherit pkgs; };

  # libfakeuid: LD_PRELOAD UID-spoofing lib used on BOTH Linux boundaries — the
  # default container path (rootless container-root spoofed to uid 1000 so the
  # store owner can mutate /nix/store while tools still refuse to run "as root")
  # and the krun microVM (host user mapped to root). krun-relay (PTY relay) is
  # microVM-only. Both ride the krunSupport gate, which is set per Linux image.
  # See lib/sandbox/linux/ for source files
  libfakeuid = pkgs.stdenv.mkDerivation {
    name = "libfakeuid";
    src = ./linux/fakeuid.c;
    dontUnpack = true;
    buildPhase = "$CC -shared -fPIC -D_GNU_SOURCE -o libfakeuid.so $src -ldl";
    installPhase = "mkdir -p $out/lib && cp libfakeuid.so $out/lib/";
  };

  krunRelay = pkgs.stdenv.mkDerivation {
    name = "krun-relay";
    src = ./linux/krun-relay.c;
    dontUnpack = true;
    buildPhase = "$CC -D_GNU_SOURCE -o krun-relay $src -lutil";
    installPhase = "mkdir -p $out/bin && cp krun-relay $out/bin/";
  };

  # Generate Claude JSON files from Nix attribute sets
  claudeConfigJson = pkgs.writeText "claude-config.json" (builtins.toJSON claudeConfig);
  claudeSettingsJson = pkgs.writeText "claude-settings.json" (builtins.toJSON claudeSettings);
  piSettingsJson = pkgs.writeText "pi-settings.json" (builtins.toJSON piSettings);

  # Per-server MCP config files for runtime selection (mcpRuntime mode)
  mcpConfigFiles = builtins.mapAttrs (
    name: config: pkgs.writeText "mcp-${name}.json" (builtins.toJSON config)
  ) mcpServerConfigs;

  # Agent runtime selection. Exactly one agent package rides the agent tier —
  # new runtimes plug in by extending the supported `agent` values. No
  # profile.pi or pi+rust special cases.
  agentPkgResolved =
    if agentPkg == null then
      throw "lib/sandbox/image.nix: agentPkg is required for agent='${agent}'"
    else
      agentPkg;
  agentPackages =
    {
      claude = [ agentPkgResolved ];
      pi = [ agentPkgResolved ];
      direct = [ agentPkgResolved ];
    }
    .${agent}
      or (throw "lib/sandbox/image.nix: unknown agent '${agent}' (expected 'claude', 'pi', or 'direct')");

  agentImageName = "wrix-agent-${agent}-${profile.name}";

  # Tier 2: exactly the one selected agent runtime and its closure, chained atop
  # the stable-profile (toolchain) tier. The leaf chains on top of this, so an
  # agent-version bump re-emits only this tier and the leaf — the heavier
  # toolchain tier below stays byte-identical (specs/image-builder.md
  # § Provenance-Tiered Layering).
  agentImage = import ./agent-image.nix {
    inherit
      pkgs
      imageBuilderPkgs
      agentPackages
      agentImageName
      stableProfileImage
      ;
  };

  # Create a merged environment with all packages for proper PATH. The agent
  # binary reaches PATH via `agentPackages` (its store path is materialized in
  # the agent tier and stripped from this leaf's graph by remove_paths).
  allPackages = [
    notifyClient
    prekWrappers.prePushChecks
    prekWrappers.skipIfMissing
  ]
  ++ (profile.packages or [ ])
  ++ agentPackages;

  profileEnv = pkgs.buildEnv {
    name = "wrix-profile-env";
    paths = allPackages;
    pathsToLink = [
      "/bin"
      "/share"
      "/etc"
      "/lib"
    ];
  };
  buildImage =
    if asTarball then
      imageBuilderPkgs.dockerTools.buildLayeredImage
    else
      imageBuilderPkgs.dockerTools.streamLayeredImage;

  materializedRoots = leafContents ++ agentImage.lowerTiersContents;

  leafContents = [
    pkgs.dockerTools.usrBinEnv
    pkgs.dockerTools.binSh
    pkgs.dockerTools.caCertificates
    profileEnv
  ];

  # The image's MATERIALIZED on-disk store, registered in the baked Nix DB so the
  # registered-valid set equals the on-disk /nix/store in BOTH directions — no
  # orphan (on-disk-but-unregistered) path, so additive in-container Nix never
  # rebuilds over a present path, AND no dangling (registered-but-absent) path,
  # so a build never trusts the DB into feeding a missing path to a builder that
  # fails with `No such file or directory` (specs/image-builder.md § In-Container
  # Nix Store Consistency).
  #
  # Two adjustments make `closureInfo` match what dockerTools actually lays down
  # (the naive full build closure both over- and under-registers):
  #   - `agentImage.lowerTiersContents` carries the MATERIALIZED set of every
  #     tier below the leaf (base + stable-profile + agent). It registers the
  #     stable tier's buildEnv INPUTS, not the `coreEnv` wrapper buildLayeredImage
  #     never lays into a store layer (registering the wrapper bakes a dangling
  #     path), and it folds in `prekHooksBundle` — which rides in `config.Env`
  #     (WRIX_PREK_HOOKS), never in any tier's `contents`, yet dockerTools still
  #     materializes its closure, so omitting it leaves an orphan (see
  #     stable-profile-image.nix § lowerTiersContents).
  #   - image-build artifacts (the customisation-layer tar, `layering.json`) are
  #     never in any `contents` or `config` reference, so they are not in this
  #     closure and never bake a dangling path.
  # The DB rides in the leaf customisation layer — registration is metadata, it
  # copies no lower-tier store path up, so the provenance-tiered chain is
  # unperturbed.
  imageNixDb = imageBuilderPkgs.closureInfo {
    rootPaths = materializedRoots;
  };

  # dockerTools builds config/layer metadata in derivations that run after the
  # image customisation layer has been produced. If automatic GC runs in that
  # gap, store paths that are only transitive references of buildEnv outputs can
  # disappear before dockerTools traverses the image graph. Keep a tiny marker
  # in the customisation layer that references every materialized image root so
  # the graph has a concrete edge to the full expected closure throughout the
  # build.
  imageStoreRoots = imageBuilderPkgs.writeText "wrix-${profile.name}-${agent}-store-roots" (
    concatStringsSep "\n" (map toString materializedRoots) + "\n"
  );

  imageName = "wrix-${profile.name}${optionalString (agent != "direct") "-${agent}"}";
  imageEntrypoint = if networkBootstrapSh == null then "/entrypoint.sh" else "/network-bootstrap.sh";

  # The leaf budgets only its tier-2 delta plus the customisation layer; with
  # base (64) and stable-profile (48) below it, this keeps the stacked image at
  # or under the 127-layer OCI ceiling (specs/image-builder.md § Base Image
  # Layering).
  maxLayers = 15;

  # The custom layeringPipeline (dockerMakeLayers) does not dedup `fromImage`
  # the way the default popularity-contest path does, so remove_paths strips the
  # UNION of all lower tiers' closures (tier 0 base + tier 1 stable-profile)
  # first — a path a lower tier already ships is never re-emitted here. Fixed
  # per-tier contents keep intra-tier ordering stable (specs/image-builder.md
  # § Base Image Layering).
  layeringPipeline =
    imageBuilderPkgs.runCommandLocal "${imageName}-layering.json"
      {
        nativeBuildInputs = [ imageBuilderPkgs.jq ];
        lowerClosure = agentImage.lowerTiersClosure;
      }
      ''
        set -euo pipefail
        jq -n \
          --rawfile storePaths "$lowerClosure/store-paths" \
          --argjson maxLayers ${toString maxLayers} \
          '($storePaths | split("\n") | map(select(length > 0))) as $lower
           | [
               [ "remove_paths", $lower ],
               [ "popularity_contest" ],
               [ "limit_layers", $maxLayers ]
             ]' \
          > "$out"
      '';

  imageConfigEnv = [
    # GIT_AUTHOR_*/GIT_COMMITTER_* set at runtime by launcher (from host git config)
    "LANG=C.UTF-8"
    "PATH=${profileEnv}/bin:/bin:/usr/bin"
    "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
    "WRIX_PREK_HOOKS=${prekHooksBundle}"
    "XDG_CACHE_HOME=/var/cache"
  ]
  ++ (mapAttrsToList (name: value: "${name}=${value}") (profile.env or { }));

  imageLabels = {
    "wrix.managed" = "true";
    "wrix.image.kind" = "profile";
    "wrix.profile.name" = profile.name;
    "wrix.agent.kind" = agent;
  };

  rawImage = buildImage {
    name = imageName;
    tag = "latest";
    architecture = pkgs.go.GOARCH;
    inherit layeringPipeline;
    includeNixDB = true;
    fromImage = agentImage;

    contents = leafContents;

    extraCommands = ''
      mkdir -p tmp home/wrix root run var/run var/cache var/tmp mnt/wrix/file mnt/wrix/dir
      chmod 1777 tmp var/cache var/tmp
      chmod 0755 run var/run
      chmod 777 home/wrix

      mkdir -p etc/wrix
      printf '%s\n' '${agent}' > etc/wrix/image-agent
      ${imageBuilderPkgs.bash}/bin/bash ${./install-known-hosts.sh} ${knownHosts}/known_hosts .
      echo "127.0.0.1 localhost" > etc/hosts

      cp ${entrypointSh} entrypoint.sh
      chmod +x entrypoint.sh

      ${optionalString (networkBootstrapSh != null) ''
        cp ${networkBootstrapSh} network-bootstrap.sh
        chmod +x network-bootstrap.sh
        mkdir -p usr/local/libexec/wrix-network
        for tool in nft iptables ip6tables capsh getent awk sort grep nc sleep; do
          ln -s "${profileEnv}/bin/$tool" "usr/local/libexec/wrix-network/$tool"
        done
      ''}

      cp ${sshConfig.gitSshSetup} git-ssh-setup.sh
      chmod 0644 git-ssh-setup.sh

      cp ${imageStoreRoots} etc/wrix/store-roots

      ${optionalString krunSupport ''
        cp ${./linux/krun-init.sh} krun-init.sh
        chmod +x krun-init.sh
        mkdir -p lib
        cp ${libfakeuid}/lib/libfakeuid.so lib/libfakeuid.so
        cp ${krunRelay}/bin/krun-relay krun-relay
        chmod +x krun-relay
      ''}

      cp ${claudeConfigJson} etc/wrix/claude-config.json
      cp ${claudeSettingsJson} etc/wrix/claude-settings.json

      ${optionalString (agent == "pi") ''
        mkdir -p etc/wrix/pi-agent
        cp ${piSettingsJson} etc/wrix/pi-agent/settings.json
      ''}

      ${optionalString (mcpServerConfigs != { }) ''
        mkdir -p etc/wrix/mcp
        ${concatStringsSep "\n" (
          mapAttrsToList (name: file: "cp ${file} etc/wrix/mcp/${name}.json") mcpConfigFiles
        )}
      ''}

      # Register the materialized on-disk closure in the nix db. includeNixDB
      # covers only the leaf's own `contents`; the fromImage tiers' materialized
      # paths (generated passwd/group/nix.conf, the stable buildEnv's inputs, the
      # config.Env-pinned prekHooksBundle, base bottom-of-closure) would otherwise
      # stay on disk but unregistered — orphans that break additive in-container
      # Nix ops. `imageNixDb` is built to match the disk exactly, so this load-db
      # registers neither an orphan nor a dangling path (specs/image-builder.md
      # § In-Container Nix Store Consistency).
      NIX_STATE_DIR=$PWD/nix/var/nix \
        ${imageBuilderPkgs.buildPackages.nix}/bin/nix-store --load-db \
        < ${imageNixDb}/registration

      # Fix Nix permissions for non-root users
      # (includeNixDB creates files owned by root)
      # Store must be writable to add new paths and create lock files
      chmod -R a+rwX nix/store nix/var/nix

      # Pre-create directory structure Nix expects with correct permissions
      # This prevents Nix from trying to chmod directories it doesn't own
      mkdir -p nix/var/nix/profiles/per-user
      mkdir -p nix/var/nix/gcroots/per-user
      mkdir -p nix/var/nix/gcroots/auto
      mkdir -p nix/var/log/nix/drvs
      chmod 755 nix/var/nix/profiles nix/var/nix/profiles/per-user
      chmod 755 nix/var/nix/gcroots nix/var/nix/gcroots/per-user
      chmod 1777 nix/var/nix/gcroots/auto
      chmod -R a+rwX nix/var/log

      # Drop Nix's 8 MiB all-zero gc-reserved-space file: opening the store
      # read-write (includeNixDB + the load-db above) creates it, it re-hashes
      # the customisation layer on any input change, and Nix recreates it
      # lazily on first in-container read-write store open.
      rm -f nix/var/nix/db/reserved
    '';

    config = {
      Env = imageConfigEnv;
      Labels = imageLabels;
      WorkingDir = "/workspace";
      Entrypoint = [ imageEntrypoint ];
    };
  };

  sourceKind = if asTarball then "docker-archive" else "nix-descriptor";

  mkOciLayout = import ./oci-layout.nix { inherit pkgs; };
  ociLayout = mkOciLayout {
    image = rawImage;
    name = "${imageName}-oci";
  };

  descriptorMetadata = {
    schema = 1;
    source_kind = "nix-descriptor";
    image = {
      name = imageName;
      tag = "latest";
    };
    profile = profile.name;
    inherit agent;
    oci_layout = "${ociLayout}";
    oci_ref = "latest";
    materialized_roots = map toString materializedRoots;
    lower_tiers_closure = "${agentImage.lowerTiersClosure}/store-paths";
    layering_pipeline = "${layeringPipeline}";
    config = {
      env = imageConfigEnv;
      labels = imageLabels;
      working_dir = "/workspace";
      entrypoint = [ imageEntrypoint ];
    };
  };
  descriptorMetadataFile = pkgs.writeText "${imageName}-descriptor-metadata.json" (
    builtins.toJSON descriptorMetadata
  );
  descriptorDigestFile = "${ociLayout}/wrix/config-digest";
  nixDescriptorSource =
    pkgs.runCommandLocal "${imageName}-nix-descriptor.json"
      {
        nativeBuildInputs = [ pkgs.jq ];
      }
      ''
        set -euo pipefail
        digest=$(cat ${descriptorDigestFile})
        jq \
          --arg digest "$digest" \
          --slurpfile ociManifest "${ociLayout}/wrix/descriptor-manifest.json" \
          '. + {
            digest: $digest,
            oci_manifest: $ociManifest[0],
            layers: ($ociManifest[0].layers // [])
          }' \
          ${descriptorMetadataFile} > "$out"
      '';
  imageSource = if asTarball then rawImage else nixDescriptorSource;

  # Darwin's tar-loadable fallback keeps using the post-conversion OCI config
  # digest that Apple's container store reports. Linux descriptor images use the
  # descriptor layout's OCI config digest without installing from a whole-image
  # archive.
  digestFile =
    if asTarball then
      hostPkgs.runCommandLocal "${imageName}-digest"
        {
          nativeBuildInputs = [
            hostPkgs.skopeo
            hostPkgs.jq
          ];
        }
        ''
          export HOME=$TMPDIR
          skopeo --insecure-policy copy --quiet \
            "docker-archive:${rawImage}" "oci:$TMPDIR/image-oci:latest"
          skopeo inspect --raw "oci:$TMPDIR/image-oci:latest" \
            | jq -r '.config.digest' > $out
        ''
    else
      descriptorDigestFile;
in
rawImage
// {
  digest = digestFile;
  source = imageSource;
  source_kind = sourceKind;
  digest_source_kind = sourceKind;
  labels = imageLabels;
  # Expose the chained `fromImage` base so callers (and the
  # base-image-hash-stable verifier, specs/image-builder.md § Base Image
  # Layering) can assert the base derivation is invariant under profile-level
  # input changes without re-deriving it.
  baseImage = wrixBaseImage;
  # Expose tier 1 so the stable-profile verifiers (hash-stability, membership,
  # pinned-toolchain) can assert against it and its `lowerTiersClosure` without
  # rebuilding the leaf, and tier 2 so the agent verifiers (tier-isolated,
  # exclusive) can assert the agent rides its own tar (specs/image-builder.md
  # § Provenance-Tiered Layering).
  inherit stableProfileImage agentImage;
  # Expose generated agent configuration so verifiers inspect the image input JSON.
  inherit
    claudeConfigJson
    claudeSettingsJson
    materializedRoots
    piSettingsJson
    ;
}
