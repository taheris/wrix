# Build the main OCI image for wrapix sandbox
#
# This creates a layered container image with:
# - Base packages + profile-specific packages
# - Claude Code package
# - Optional consumer-supplied `piPkg` when `agent == "pi"`
# - Optional consumer-supplied `directRunner` when `agent == "direct"`
# - CA certificates for HTTPS
# - Platform-specific entrypoint script
#
# The image is composed from two orthogonal axes:
#   - workspace profile (base | rust | python) — toolchain packages
#   - agent runtime (claude | pi | direct) — agent binary layer
#
# Claude is always present (it's part of the base image today), so the claude
# runtime layer is a no-op. The pi runtime layer adds the consumer-supplied
# `piPkg` (e.g. loom's `pi-mono` derivation); the direct runtime layer adds
# the consumer-supplied `directRunner` package (e.g. loom's
# `loom-direct-runner`) so the entrypoint can exec it over JSONL stdio on
# WRAPIX_AGENT=direct. Wrapix no longer ships pi-mono in tree.
#
# Layer ordering: stable packages first, frequently-changing packages last.
# This maximizes layer cache hits across rebuilds and profiles.
{
  pkgs,
  hostPkgs ? pkgs,
  profile,
  entrypointPkg,
  entrypointSh,
  krunSupport ? false,
  claudeConfig,
  claudeSettings,
  mcpServerConfigs ? { },
  # Agent runtime axis. "claude" (default) is a no-op; "pi" adds the
  # consumer-supplied `piPkg` for `pi --mode rpc`; "direct" adds the
  # consumer-supplied direct-runner binary that the entrypoint execs over
  # JSONL stdio.
  agent ? "claude",
  # Linux-built package whose `bin/` directory contains the direct-runner
  # binary. Required when `agent == "direct"`; ignored otherwise. Consumers
  # provide this themselves (e.g. via `loom.packages.${system}.default`)
  # since wrapix no longer builds loom in-tree.
  directRunner ? null,
  # Linux-built package whose `bin/` directory contains the `pi` binary.
  # Required when `agent == "pi"`; ignored otherwise. Consumers provide this
  # themselves since wrapix no longer ships pi-mono in tree.
  piPkg ? null,
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

  notifyClient = import ../notify/client.nix { inherit pkgs; };

  # Shared nixpkgs-pin-dependent bottom-of-closure. Chained under the
  # per-profile image via `fromImage` so it loads into the platform store once
  # (specs/image-builder.md § Base Image Layering).
  wrapixBaseImage = import ./base-image.nix { inherit pkgs; };

  # Tier 1: the fixed-per-instance closure (profile.corePackages + the
  # wrapix-generated derivations that do not vary with profile.packages, MCP
  # configs, Claude settings, or agent selection), chained atop the base. The
  # leaf chains on top of this via `fromImage`, so this tar loads into the
  # platform store once (specs/image-builder.md § Base Image Layering).
  stableProfileImage = import ./stable-profile-image.nix {
    inherit pkgs profile;
    claudePkg = entrypointPkg;
  };

  # Bundle referenced from config.Env WRAPIX_PREK_HOOKS so the entrypoint can
  # point `core.hooksPath` at it (specs/pre-commit.md § Bead-Container Hook
  # Installation).
  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };

  # `pre-push-checks` and `skip-if-missing` wrappers — co-located on PATH so
  # `.pre-commit-config.yaml` entries that name them resolve inside the
  # container (specs/pre-commit.md § Hook-Entry Wrappers).
  prekWrappers = import ../prek/wrappers.nix { inherit pkgs; };

  # krun microVM support: UID spoofing library + PTY relay
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

  # Per-server MCP config files for runtime selection (mcpRuntime mode)
  mcpConfigFiles = builtins.mapAttrs (
    name: config: pkgs.writeText "mcp-${name}.json" (builtins.toJSON config)
  ) mcpServerConfigs;

  # Agent runtime layer. `claude` is a no-op (claudeCode is the entrypointPkg
  # already baked into every image); `pi` adds the consumer-supplied `piPkg`;
  # `direct` adds the consumer-supplied `directRunner` package. New runtimes
  # plug in by extending this lookup — no profile.pi or pi+rust special cases.
  directRunnerPkg =
    if directRunner == null then
      throw "lib/sandbox/image.nix: agent='direct' requires the `directRunner` argument"
    else
      directRunner;
  piPkgResolved = if piPkg == null then throw ''mkSandbox: agent = "pi" requires piPkg'' else piPkg;
  agentPackages =
    {
      claude = [ ];
      pi = [ piPkgResolved ];
      direct = [ directRunnerPkg ];
    }
    .${agent}
      or (throw "lib/sandbox/image.nix: unknown agent '${agent}' (expected 'claude', 'pi', or 'direct')");

  # Create a merged environment with all packages for proper PATH
  allPackages = [
    entrypointPkg
    notifyClient
    prekWrappers.prePushChecks
    prekWrappers.skipIfMissing
  ]
  ++ (profile.packages or [ ])
  ++ agentPackages;

  profileEnv = pkgs.buildEnv {
    name = "wrapix-profile-env";
    paths = allPackages;
    pathsToLink = [
      "/bin"
      "/share"
      "/etc"
      "/lib"
    ];
  };
  buildImage =
    if asTarball then pkgs.dockerTools.buildLayeredImage else pkgs.dockerTools.streamLayeredImage;

  imageName = "wrapix-${profile.name}${pkgs.lib.optionalString (agent != "claude") "-${agent}"}";

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
    pkgs.runCommandLocal "${imageName}-layering.json"
      {
        nativeBuildInputs = [ pkgs.jq ];
        lowerClosure = stableProfileImage.lowerTiersClosure;
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

  rawImage = buildImage {
    name = imageName;
    tag = "latest";
    inherit layeringPipeline;
    includeNixDB = true;
    fromImage = stableProfileImage;

    contents = [
      pkgs.dockerTools.usrBinEnv
      pkgs.dockerTools.binSh
      pkgs.dockerTools.caCertificates
      profileEnv
    ];

    extraCommands = ''
      mkdir -p tmp home/wrapix root var/run var/cache var/tmp mnt/wrapix/file mnt/wrapix/dir
      chmod 1777 tmp var/cache var/tmp
      chmod 777 home/wrapix

      mkdir -p etc/wrapix
      echo "127.0.0.1 localhost" > etc/hosts

      cp ${entrypointSh} entrypoint.sh
      chmod +x entrypoint.sh

      cp ${sshConfig.gitSshSetup} git-ssh-setup.sh
      chmod 0644 git-ssh-setup.sh

      ${pkgs.lib.optionalString krunSupport ''
        cp ${./linux/krun-init.sh} krun-init.sh
        chmod +x krun-init.sh
        mkdir -p lib
        cp ${libfakeuid}/lib/libfakeuid.so lib/libfakeuid.so
        cp ${krunRelay}/bin/krun-relay krun-relay
        chmod +x krun-relay
      ''}

      cp ${claudeConfigJson} etc/wrapix/claude-config.json
      cp ${claudeSettingsJson} etc/wrapix/claude-settings.json

      ${optionalString (mcpServerConfigs != { }) ''
        mkdir -p etc/wrapix/mcp
        ${concatStringsSep "\n" (
          mapAttrsToList (name: file: "cp ${file} etc/wrapix/mcp/${name}.json") mcpConfigFiles
        )}
      ''}

      # Register prekHooksBundle in nix db — referenced only from config.Env
      # (WRAPIX_PREK_HOOKS), which includeNixDB does not cover.
      NIX_STATE_DIR=$PWD/nix/var/nix \
        ${pkgs.buildPackages.nix}/bin/nix-store --load-db \
        < ${pkgs.closureInfo { rootPaths = [ prekHooksBundle ]; }}/registration

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
      Env = [
        # GIT_AUTHOR_*/GIT_COMMITTER_* set at runtime by launcher (from host git config)
        "LANG=C.UTF-8"
        "PATH=${profileEnv}/bin:/bin:/usr/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "WRAPIX_PREK_HOOKS=${prekHooksBundle}"
        "XDG_CACHE_HOME=/var/cache"
      ]
      ++ (mapAttrsToList (name: value: "${name}=${value}") (profile.env or { }));
      WorkingDir = "/workspace";
      Entrypoint = [ "/entrypoint.sh" ];
    };
  };

  # Content digest of the OCI config blob (== podman's `.Id` / the Apple
  # container CLI's image digest), extracted at build time so the launcher's
  # preflight (specs/sandbox.md § Image install path) can decide whether the
  # image is already present without re-streaming or invoking *-load. Stable
  # across drv-hash rebuilds when the image content is unchanged — which
  # `mkImageRef`'s drv-hash tag cannot detect.
  #
  # The digest MUST be computed through the same Docker schema2 → OCI
  # conversion the launcher's install transport runs: skopeo rewrites the
  # config blob on conversion, so the docker-archive manifest's `.Config`
  # digest does NOT equal the config digest of the stored OCI image. Reading
  # the post-conversion OCI config digest here is what makes the preflight a
  # real hit instead of a guaranteed miss that re-streams on every launch.
  #
  # The conversion targets the `oci:` directory layout, not `oci-archive:`:
  # the archive transport hardcodes `/var/tmp` for its assembly temp dir
  # (ignoring `TMPDIR`), which does not exist in the Nix build sandbox. The
  # directory layout writes blobs in place and yields the identical config
  # digest the launcher's `oci-archive:` transport stores as podman's `.Id`.
  digestFile =
    hostPkgs.runCommandLocal "${imageName}-digest"
      {
        nativeBuildInputs = [
          hostPkgs.skopeo
          hostPkgs.jq
        ];
      }
      (
        ''
          export HOME=$TMPDIR
        ''
        + (
          if asTarball then
            ''
              skopeo --insecure-policy copy --quiet \
                "docker-archive:${rawImage}" "oci:$TMPDIR/image-oci:latest"
            ''
          else
            ''
              ${rawImage} > "$TMPDIR/image.tar"
              skopeo --insecure-policy copy --quiet \
                "docker-archive:$TMPDIR/image.tar" "oci:$TMPDIR/image-oci:latest"
            ''
        )
        + ''
          skopeo inspect --raw "oci:$TMPDIR/image-oci:latest" \
            | jq -r '.config.digest' > $out
        ''
      );
in
rawImage
// {
  digest = digestFile;
  # Expose the chained `fromImage` base so callers (and the
  # base-image-hash-stable verifier, specs/image-builder.md § Base Image
  # Layering) can assert the base derivation is invariant under profile-level
  # input changes without re-deriving it.
  baseImage = wrapixBaseImage;
  # Expose tier 1 so the stable-profile verifiers (hash-stability, membership,
  # pinned-toolchain) can assert against the middle tier's derivation and its
  # `lowerTiersClosure` without rebuilding the leaf (specs/image-builder.md
  # § Provenance-Tiered Layering).
  inherit stableProfileImage;
}
