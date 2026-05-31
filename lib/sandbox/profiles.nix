{
  pkgs,
  hostPkgs ? pkgs,
  crane ? null,
  fenix ? null,
  treefmt ? null,
}:

let
  # Base packages included in all profiles
  basePackages = with pkgs; [
    bash
    beads
    beads-push
    coreutils
    curl
    diffutils
    dolt
    fd
    file
    findutils
    gawk
    gh
    git
    gnugrep
    gnused
    gnutar
    gzip
    iproute2
    iptables
    iputils
    jq
    less
    lsof
    man
    netcat
    nix
    openssh
    patch
    prek
    procps
    python3
    ripgrep
    rsync
    shellcheck
    sqlite
    tmux
    tree
    treefmtPkg
    unzip
    util-linux
    vim
    whichQuiet
    yq
    zip
  ];

  # Required mounts for all profiles
  # Note: Host ~/.claude is NOT mounted - containers use $PROJECT_DIR/.claude instead
  # This isolates containers from host config while persisting sessions in the project
  baseMounts = [ ];

  # Environment variables in all profiles
  baseEnv = { };

  # Base network allowlist for WRAPIX_NETWORK=limit mode
  # These domains are always permitted regardless of profile
  baseNetworkAllowlist = [
    "api.anthropic.com" # Claude API
    "cache.nixos.org" # Nix binary cache
    "github.com" # git operations
    "ssh.github.com" # git SSH (port 443 fallback)
  ];

  # Helper to create a profile with base packages, mounts, and env merged in.
  # shellHook is a shell snippet a downstream host devShell splices in to
  # align host-side toolchain identity,
  # env, and PATH with the sandbox — e.g. prepending the rust profile's
  # `${toolchain}/bin` so host `rustc` resolves to the same /nix/store/... path
  # the sandbox uses, the prerequisite for cross-boundary sccache hits.
  mkProfile =
    {
      name,
      packages ? [ ],
      env ? { },
      mounts ? [ ],
      networkAllowlist ? [ ],
      enabledPlugins ? { },
      shellHook ? "",
      writableDirs ? [ ],
    }:
    {
      inherit
        name
        enabledPlugins
        shellHook
        writableDirs
        ;
      packages = basePackages ++ packages;
      env = baseEnv // env;
      mounts = baseMounts ++ mounts;
      networkAllowlist = baseNetworkAllowlist ++ networkAllowlist;
    };

  requireFenix =
    if fenix == null then
      throw "lib/sandbox/profiles.nix: profiles.rust requires the fenix input; pass `fenix` to this file"
    else
      fenix;

  # Two fenix package sets so the rust profile can serve both the image (always
  # Linux) and host-platform consumers (devshell, buildPackage) without forcing
  # a cross-build. On Linux hosts both resolve to the same /nix/store path; on
  # Darwin they diverge (image stays Linux, host gets Darwin).
  imageFenixPkgs = requireFenix.packages.${pkgs.stdenv.hostPlatform.system};
  hostFenixPkgs = requireFenix.packages.${hostPkgs.stdenv.hostPlatform.system};

  mkRustToolchain =
    fenixSet: base:
    fenixSet.combine [
      base
      # stable RA from manifest avoids dragging matching nightly toolchain into closure
      fenixSet.stable.rust-analyzer-preview
      fenixSet.stable.rust-src
    ];

  # fenix's minimalToolchain omits clippy/rustfmt; defaultToolchain is the
  # rustup-equivalent "default" set (rustc + cargo + rust-std + clippy + rustfmt + rust-docs).
  defaultImageToolchain = mkRustToolchain imageFenixPkgs imageFenixPkgs.stable.defaultToolchain;
  defaultHostToolchain = mkRustToolchain hostFenixPkgs hostFenixPkgs.stable.defaultToolchain;

  # crane.mkLib is bound to hostPkgs so buildPackage produces host-platform
  # binaries (the devshell + `nix build .#tmux-mcp` path). The in-image build
  # uses a separate profile instance constructed with hostPkgs = pkgs; see
  # lib/sandbox/default.nix.
  mkCraneLib =
    toolchain:
    if crane == null then
      throw "lib/sandbox/profiles.nix: profiles.rust requires the crane input; pass `crane` to this file"
    else
      (crane.mkLib hostPkgs).overrideToolchain (_: toolchain);

  # bin closes over src + cargoArtifacts only — no edge to clippy/nextest, no
  # extraSrcs — so devshell consumers stay warm across test-fixture edits.
  mkBuildPackageFn =
    craneLib:
    {
      src,
      cargoLock,
      extraSrcs ? { },
      cargoArtifacts ? null,
      cargoExtraArgs ? "",
      buildInputs ? [ ],
      propagatedBuildInputs ? [ ],
      nativeBuildInputs ? [ ],
      meta ? { },
      srcFilter ? null,
    }:
    let
      cleanedSrc =
        if srcFilter == null then
          craneLib.cleanCargoSource src
        else
          pkgs.lib.cleanSourceWith {
            inherit src;
            filter = srcFilter;
          };

      commonArgs = {
        src = cleanedSrc;
        inherit
          cargoLock
          cargoExtraArgs
          buildInputs
          propagatedBuildInputs
          nativeBuildInputs
          meta
          ;
      };

      resolvedCargoArtifacts =
        if cargoArtifacts != null then cargoArtifacts else craneLib.buildDepsOnly commonArgs;

      stagedSrc =
        if extraSrcs == { } then
          cleanedSrc
        else
          pkgs.runCommand "rust-src-with-extras" { } (
            ''
              cp -r ${cleanedSrc} $out
              chmod -R u+w $out
            ''
            + pkgs.lib.concatStringsSep "\n" (
              pkgs.lib.mapAttrsToList (rel: abs: ''
                mkdir -p "$(dirname "$out/${rel}")"
                cp -r ${abs} "$out/${rel}"
              '') extraSrcs
            )
          );

      bin = craneLib.buildPackage (
        commonArgs
        // {
          cargoArtifacts = resolvedCargoArtifacts;
          doCheck = false;
        }
      );

      clippy = craneLib.cargoClippy (
        commonArgs
        // {
          src = stagedSrc;
          cargoArtifacts = resolvedCargoArtifacts;
          cargoClippyExtraArgs = "--all-targets";
        }
      );

      nextest = craneLib.cargoNextest (
        commonArgs
        // {
          src = stagedSrc;
          cargoArtifacts = resolvedCargoArtifacts;
        }
      );
    in
    {
      inherit bin clippy nextest;
      cargoArtifacts = resolvedCargoArtifacts;
    };

  # Build a Rust profile attrset from a given (image-toolchain, host-toolchain) pair.
  # The image toolchain lands in profile.packages and the image-side env exports;
  # the host toolchain backs profile.toolchain, the devshell PATH prepend, and
  # buildPackage's craneLib. On Linux hosts the two coincide.
  mkRustProfile =
    {
      imageToolchain,
      hostToolchain,
    }:
    mkProfile {
      name = "rust";

      packages = [
        imageToolchain
        pkgs.gcc
        pkgs.openssl
        pkgs.openssl.dev
        pkgs.pkg-config
        pkgs.postgresql.lib
        pkgs.sccache
      ];

      enabledPlugins = {
        "rust-analyzer-lsp@claude-plugins-official" = true;
      };

      env = {
        CARGO_BUILD_RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";
        CARGO_INCREMENTAL = "0";
        LIBRARY_PATH = "${pkgs.postgresql.lib}/lib";
        OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
        OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
        RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";
        RUST_SRC_PATH = "${imageToolchain}/lib/rustlib/src/rust/library";
        SCCACHE_CACHE_SIZE = "50G";
        SCCACHE_DIR = "/home/wrapix/.cache/sccache";
      };

      mounts = [
        {
          source = "~/.cargo/registry";
          dest = "/home/wrapix/.cargo/registry";
          mode = "rw";
          optional = true;
        }
        {
          source = "~/.cargo/git";
          dest = "/home/wrapix/.cargo/git";
          mode = "rw";
          optional = true;
        }
        {
          source = "~/.cache/sccache";
          dest = "/home/wrapix/.cache/sccache";
          mode = "rw";
          optional = true;
        }
      ];

      networkAllowlist = [
        "crates.io"
        "static.crates.io"
        "index.crates.io"
      ];

      # Linux-only: stack a tmpfs at CARGO_HOME and .cache so the dirs are
      # wrapix-owned. Without this, podman creates them as root to anchor the
      # registry/git/sccache binds, and cargo/sccache (as wrapix) can't write
      # their own files there. The .cache tmpfs also keeps sccache functional
      # when the optional ~/.cache/sccache host mount is absent.
      # Darwin is unaffected: the entrypoint mkdirs these paths itself as
      # namespaced-root-mapped-to-HOST_UID, so they're already writable.
      writableDirs = [
        "/home/wrapix/.cargo"
        "/home/wrapix/.cache"
      ];

      # Align host with the sandbox so cross-boundary cache hits work.
      # PATH prepend pins host `rustc` to the same fenix derivation the sandbox
      # bakes in — without it, host falls through to rustup and the diverging
      # sysroot baked into rlib metadata invalidates every sccache key.
      #
      # SCCACHE_DIR must stay set in profile.env (the image's
      # XDG_CACHE_HOME=/var/cache would otherwise send sccache to
      # /var/cache/sccache, missing the host mount). On the host devshell,
      # mkDevShell merges profile.env into mkShell's env, so the container
      # path /home/wrapix/.cache/sccache leaks into the host. Match-and-unset
      # so the :- default falls through to $HOME/.cache/sccache; user-set
      # overrides survive.
      shellHook = ''
        [ "''${SCCACHE_DIR:-}" = "/home/wrapix/.cache/sccache" ] && unset SCCACHE_DIR
        export PATH="${hostToolchain}/bin:$PATH"
        export RUSTC_WRAPPER="${hostPkgs.sccache}/bin/sccache"
        export CARGO_BUILD_RUSTC_WRAPPER="${hostPkgs.sccache}/bin/sccache"
        export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
        export SCCACHE_CACHE_SIZE="''${SCCACHE_CACHE_SIZE:-50G}"
        export CARGO_INCREMENTAL="''${CARGO_INCREMENTAL:-0}"
      '';
    }
    // (
      let
        craneLib = mkCraneLib hostToolchain;
      in
      {
        toolchain = hostToolchain;
        inherit craneLib;
        buildPackage = mkBuildPackageFn craneLib;
      }
    );

  # Project-wide formatter wrapper. Forced when basePackages is materialized
  treefmtPkg =
    if treefmt == null then
      throw "lib/sandbox/profiles.nix: basePackages requires the treefmt wrapper; pass `treefmt` to this file"
    else
      treefmt;

  # Suppress GNU which's verbose "no X in (PATH)" errors
  whichQuiet = pkgs.writeShellScriptBin "which" ''
    ${pkgs.which}/bin/which "$@" 2>/dev/null
  '';

  # Build a project-pinned rust profile from a rust-toolchain.toml file.
  # Used by lib/default.nix's wrapix.rustProfile; not exposed as part of the
  # public profiles attrset surface (sandbox/default.nix strips it before
  # re-export).
  rustProfileFromFile =
    { file, sha256 }:
    mkRustProfile {
      imageToolchain = mkRustToolchain imageFenixPkgs (
        imageFenixPkgs.fromToolchainFile { inherit file sha256; }
      );
      hostToolchain = mkRustToolchain hostFenixPkgs (
        hostFenixPkgs.fromToolchainFile { inherit file sha256; }
      );
    };

in
{
  base = mkProfile {
    name = "base";
  };

  rust = mkRustProfile {
    imageToolchain = defaultImageToolchain;
    hostToolchain = defaultHostToolchain;
  };

  inherit rustProfileFromFile;

  python = mkProfile {
    name = "python";

    packages = with pkgs; [
      ruff
      ty
      uv
    ];

    env = {
      UV_CACHE_DIR = "/home/wrapix/.cache/uv";
    };

    mounts = [
      {
        source = "~/.cache/uv";
        dest = "/home/wrapix/.cache/uv";
        mode = "rw";
        optional = true;
      }
    ];

    networkAllowlist = [
      "pypi.org"
      "files.pythonhosted.org"
    ];
  };
}
