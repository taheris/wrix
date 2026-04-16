{ pkgs }:

let
  # Build a Rust toolchain from rust-overlay with required extensions
  mkRustToolchain =
    base:
    base.override {
      extensions = [
        "rust-src"
        "rust-analyzer"
      ];
    };

  # Default stable toolchain with extensions
  defaultRustToolchain = mkRustToolchain pkgs.rust-bin.stable.latest.default;

  # Build a Rust profile attrset from a given toolchain
  mkRustProfile =
    toolchain:
    let
      profile = mkProfile {
        name = "rust";

        packages = [
          toolchain
          pkgs.gcc
          pkgs.openssl
          pkgs.openssl.dev
          pkgs.pkg-config
          pkgs.postgresql.lib
        ];

        enabledPlugins = {
          "rust-analyzer-lsp@claude-plugins-official" = true;
        };

        env = {
          CARGO_HOME = "/workspace/.cargo";
          RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
          LIBRARY_PATH = "${pkgs.postgresql.lib}/lib";
          OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
          OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
        };

        mounts = [
          {
            source = "~/.cargo/registry";
            dest = "~/.cargo/registry";
            mode = "ro";
            optional = true;
          }
          {
            source = "~/.cargo/git";
            dest = "~/.cargo/git";
            mode = "ro";
            optional = true;
          }
        ];

        networkAllowlist = [
          "crates.io"
          "static.crates.io"
          "index.crates.io"
        ];
      };
    in
    profile;

  # Build a toolchain from a rust-toolchain.toml file, ensuring rust-src and rust-analyzer
  withToolchainFromFile =
    toolchainFile:
    let
      base = pkgs.rust-bin.fromRustupToolchainFile toolchainFile;
      toolchain = mkRustToolchain base;
    in
    mkRustProfile toolchain;

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

  # Helper to create a profile with base packages, mounts, and env merged in
  mkProfile =
    {
      name,
      packages ? [ ],
      env ? { },
      mounts ? [ ],
      networkAllowlist ? [ ],
      enabledPlugins ? { },
    }:
    {
      inherit name enabledPlugins;
      packages = basePackages ++ packages;
      env = baseEnv // env;
      mounts = baseMounts ++ mounts;
      networkAllowlist = baseNetworkAllowlist ++ networkAllowlist;
    };

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
