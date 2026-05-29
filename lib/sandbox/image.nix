# Build the main OCI image for wrapix sandbox
#
# This creates a layered container image with:
# - Base packages + profile-specific packages
# - Claude Code package
# - Optional pi-mono runtime (Node.js + pi binary) when `agent == "pi"`
# - Optional Direct runtime binary when `agent == "direct"`
# - CA certificates for HTTPS
# - Platform-specific entrypoint script
#
# The image is composed from two orthogonal axes:
#   - workspace profile (base | rust | python) — toolchain packages
#   - agent runtime (claude | pi | direct) — agent binary layer
#
# Claude is always present (it's part of the base image today), so the claude
# runtime layer is a no-op. The pi runtime layer adds pkgs.pi-mono. The
# direct runtime layer adds a consumer-supplied `directRunner` package
# (e.g. loom's `loom-direct-runner`) so the entrypoint can exec it over
# JSONL stdio on WRAPIX_AGENT=direct.
#
# Layer ordering: stable packages first, frequently-changing packages last.
# This maximizes layer cache hits across rebuilds and profiles.
{
  pkgs,
  profile,
  entrypointPkg,
  entrypointSh,
  krunSupport ? false,
  claudeConfig,
  claudeSettings,
  mcpServerConfigs ? { },
  # Agent runtime axis. "claude" (default) is a no-op; "pi" adds pi-mono
  # (Node.js + pi binary) for `pi --mode rpc`; "direct" adds the
  # consumer-supplied direct-runner binary that the entrypoint execs over
  # JSONL stdio.
  agent ? "claude",
  # Linux-built package whose `bin/` directory contains the direct-runner
  # binary. Required when `agent == "direct"`; ignored otherwise. Consumers
  # provide this themselves (e.g. via `loom.packages.${system}.default`)
  # since wrapix no longer builds loom in-tree.
  directRunner ? null,
  # Use buildLayeredImage (tar in store) instead of streamLayeredImage (script).
  # Required on Darwin where the stream script's Linux Python shebang won't execute.
  asTarball ? false,
}:

let
  inherit (pkgs.lib) concatStringsSep mapAttrsToList optionalString;
  sshConfig = import ../util/ssh.nix;

  notifyClient = import ../notify/client.nix { inherit pkgs; };

  # Bundle referenced from config.Env WRAPIX_PREK_HOOKS so the entrypoint can
  # point `core.hooksPath` at it (specs/pre-commit.md § Bead-Container Hook
  # Installation).
  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };

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

  # Nix sandbox disabled: outer container provides isolation.
  # See specs/image-builder.md for the rationale.
  nixConfig = pkgs.writeTextDir "etc/nix/nix.conf" ''
    experimental-features = nix-command flakes
    sandbox = false
    filter-syscalls = false
  '';

  # Generate Claude JSON files from Nix attribute sets
  claudeConfigJson = pkgs.writeText "claude-config.json" (builtins.toJSON claudeConfig);
  claudeSettingsJson = pkgs.writeText "claude-settings.json" (builtins.toJSON claudeSettings);

  # Per-server MCP config files for runtime selection (mcpRuntime mode)
  mcpConfigFiles = builtins.mapAttrs (
    name: config: pkgs.writeText "mcp-${name}.json" (builtins.toJSON config)
  ) mcpServerConfigs;

  # Base passwd/group with fixed wrapix user (UID remapped at runtime via --userns=keep-id or setpriv)
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

  # Agent runtime layer. `claude` is a no-op (claudeCode is the entrypointPkg
  # already baked into every image); `pi` adds pi-mono; `direct` adds the
  # consumer-supplied `directRunner` package. New runtimes plug in by
  # extending this lookup — no profile.pi or pi+rust special cases.
  directRunnerPkg =
    if directRunner == null then
      throw "lib/sandbox/image.nix: agent='direct' requires the `directRunner` argument"
    else
      directRunner;
  agentPackages =
    {
      claude = [ ];
      pi = [ pkgs.pi-mono ];
      direct = [ directRunnerPkg ];
    }
    .${agent}
      or (throw "lib/sandbox/image.nix: unknown agent '${agent}' (expected 'claude', 'pi', or 'direct')");

  # Create a merged environment with all packages for proper PATH
  allPackages = [
    entrypointPkg
    notifyClient
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
in
buildImage {
  name = "wrapix-${profile.name}${pkgs.lib.optionalString (agent != "claude") "-${agent}"}";
  tag = "latest";
  maxLayers = 100;
  includeNixDB = true;

  contents = [
    passwdFile
    groupFile
    pkgs.dockerTools.usrBinEnv
    pkgs.dockerTools.binSh
    pkgs.dockerTools.caCertificates
    pkgs.cacert
    nixConfig
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
}
