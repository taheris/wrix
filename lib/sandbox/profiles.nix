{
  pkgs,
  hostPkgs ? pkgs,
  fenix ? null,
  treefmt ? null,
}:

let
  # Agent tooling: project-wide formatter wrapper. Forced when basePackages
  # is materialized; throws with a helpful message if the caller didn't thread
  # the treefmt-nix wrapper through from the flake.
  treefmtPkg =
    if treefmt == null then
      throw "lib/sandbox/profiles.nix: basePackages requires the treefmt wrapper; pass `treefmt` to this file"
    else
      treefmt;

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
    gc
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
    ripgrep
    rsync
    shellcheck
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
    "github.com" # git operations
    "ssh.github.com" # git SSH (port 443 fallback)
    "cache.nixos.org" # Nix binary cache
  ];

  # Helper to create a profile with base packages, mounts, and env merged in.
  # hostShellHook is a shell snippet consumer shellHooks (ralph, city, or a
  # downstream devShell) splice in to align host-side env with what the profile
  # mounts into the sandbox — e.g. pointing host sccache at the path the rust
  # profile mounts, so cross-boundary cache hits work.
  mkProfile =
    {
      name,
      packages ? [ ],
      env ? { },
      mounts ? [ ],
      networkAllowlist ? [ ],
      enabledPlugins ? { },
      hostShellHook ? "",
      writableDirs ? [ ],
    }:
    {
      inherit
        name
        enabledPlugins
        hostShellHook
        writableDirs
        ;
      packages = basePackages ++ packages;
      env = baseEnv // env;
      mounts = baseMounts ++ mounts;
      networkAllowlist = baseNetworkAllowlist ++ networkAllowlist;
    };

  fenixPkgs =
    if fenix == null then
      throw "lib/sandbox/profiles.nix: profiles.rust requires the fenix input; pass `fenix` to this file"
    else
      fenix.packages.${pkgs.stdenv.hostPlatform.system};

  mkRustToolchain =
    base:
    fenixPkgs.combine [
      base
      fenixPkgs.rust-analyzer
      fenixPkgs.stable.rust-src
    ];

  # fenix's minimalToolchain omits clippy/rustfmt; defaultToolchain is the
  # rustup-equivalent "default" set (rustc + cargo + rust-std + clippy + rustfmt + rust-docs).
  defaultRustToolchain = mkRustToolchain fenixPkgs.stable.defaultToolchain;

  # Build a Rust profile attrset from a given toolchain
  mkRustProfile =
    toolchain:
    mkProfile {
      name = "rust";

      packages = [
        toolchain
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
        CARGO_HOME = "/home/wrapix/.cargo";
        CARGO_INCREMENTAL = "0";
        LIBRARY_PATH = "${pkgs.postgresql.lib}/lib";
        OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
        OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
        RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";
        RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
        SCCACHE_CACHE_SIZE = "50G";
        SCCACHE_DIR = "/home/wrapix/.cache/sccache";
      };

      mounts = [
        {
          source = "~/.cargo/registry";
          dest = "/home/wrapix/.cargo/registry";
          mode = "ro";
          optional = true;
        }
        {
          source = "~/.cargo/git";
          dest = "/home/wrapix/.cargo/git";
          mode = "ro";
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

      # Linux-only: stack a tmpfs at CARGO_HOME so the dir is wrapix-owned.
      # Without this, podman creates /home/wrapix/.cargo as root to serve as
      # the mountpoint parent for the ro registry/git binds, and cargo (as
      # wrapix) can't write its .global-cache/credentials there.
      # Darwin is unaffected: the entrypoint mkdirs these paths itself as
      # namespaced-root-mapped-to-HOST_UID, so they're already writable.
      writableDirs = [ "/home/wrapix/.cargo" ];

      # Align host with the sandbox so cross-boundary cache hits work.
      # RUSTC_WRAPPER: always pin to wrapix's sccache so host and sandbox
      # run the same version — avoids cache-format drift if the user's
      # ambient sccache is a different build.
      # SCCACHE_DIR/SCCACHE_CACHE_SIZE/CARGO_INCREMENTAL: default if unset;
      # downstream can override in its own shellHook.
      hostShellHook = ''
        export RUSTC_WRAPPER="${hostPkgs.sccache}/bin/sccache"
        export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
        export SCCACHE_CACHE_SIZE="''${SCCACHE_CACHE_SIZE:-50G}"
        export CARGO_INCREMENTAL="''${CARGO_INCREMENTAL:-0}"
      '';
    };

  # Build a toolchain from a rust-toolchain.toml file. fenix requires a sha256
  # of the downloaded components for purity, so callers must pass { file, sha256 }.
  withToolchainFromFile =
    { file, sha256 }:
    let
      base = fenixPkgs.fromToolchainFile { inherit file sha256; };
      toolchain = mkRustToolchain base;
    in
    mkRustProfile toolchain;

  # Suppress GNU which's verbose "no X in (PATH)" errors
  whichQuiet = pkgs.writeShellScriptBin "which" ''
    ${pkgs.which}/bin/which "$@" 2>/dev/null
  '';

in
{
  base = mkProfile {
    name = "base";
  };

  rust = mkRustProfile defaultRustToolchain // {
    withToolchain = withToolchainFromFile;
  };

  python = mkProfile {
    name = "python";

    packages = with pkgs; [
      python3
      ruff
      ty
      uv
    ];

    env = {
      UV_CACHE_DIR = "/workspace/.uv-cache";
    };

    mounts = [
      {
        source = "~/.cache/uv";
        dest = "~/.cache/uv";
        mode = "ro";
        optional = true;
      }
    ];

    networkAllowlist = [
      "pypi.org"
      "files.pythonhosted.org"
    ];
  };
}
