# Build the OCI image for wrix-builder (Linux remote builder)
#
# This creates a layered container image with:
# - nix-daemon for remote building
# - sshd for ssh-ng:// access
# - Builder user with UID 1000 for VirtioFS compatibility
#
{ pkgs }:

let
  # Static busybox for bootstrapping when /nix volume is empty
  # Provides /bin/sh and basic commands independent of /nix/store
  inherit (pkgs.pkgsStatic) busybox;

  # Packages needed for remote building
  builderPackages = with pkgs; [
    nix
    openssh
    coreutils
    bash
    gnugrep
    gnutar
    gzip
    xz
    git
    cacert
    procps # for pgrep, ps, etc.
  ];

  # passwd: root, builder (UID 1000), sshd (for privilege separation), nobody
  passwdFile = pkgs.writeTextDir "etc/passwd" ''
    root:x:0:0:root:/root:/bin/bash
    builder:x:1000:1000:Nix Builder:/home/builder:/bin/bash
    sshd:x:74:74:Privilege-separated SSH:/var/empty:/bin/false
    nobody:x:65534:65534:Unprivileged account:/var/empty:/bin/false
  '';

  # group: root, users (with builder), sshd, nogroup
  groupFile = pkgs.writeTextDir "etc/group" ''
    root:x:0:
    users:x:100:builder
    sshd:x:74:
    nogroup:x:65534:
  '';

  # sshd configuration: key-only auth, allow builder user
  # Note: entrypoint.sh regenerates this at runtime to handle persistent store mounts
  sshdConfig = pkgs.writeTextDir "etc/ssh/sshd_config" ''
    Port 22
    ListenAddress 127.0.0.1
    HostKey /etc/ssh/ssh_host_ed25519_key
    AuthorizedKeysFile /home/%u/.ssh/authorized_keys
    PasswordAuthentication no
    PermitRootLogin no
    AllowUsers builder
    Subsystem sftp internal-sftp
  '';

  # Create merged environment with all packages
  builderEnv = pkgs.buildEnv {
    name = "wrix-builder-env";
    paths = builderPackages;
    pathsToLink = [
      "/bin"
      "/share"
      "/etc"
      "/lib"
    ];
  };

in
pkgs.dockerTools.buildLayeredImage {
  name = "wrix-builder";
  tag = "latest";
  maxLayers = 50;
  includeNixDB = true;

  contents = [
    passwdFile
    groupFile
    sshdConfig
    pkgs.dockerTools.usrBinEnv
    # Note: binSh omitted - we use static busybox instead so /bin/sh works
    # even when /nix is mounted empty for persistent store initialization
    pkgs.dockerTools.caCertificates
    builderEnv
  ];

  extraCommands = ''
    mkdir -p tmp var/run var/log var/empty home/builder/.ssh run/sshd etc/ssh bin
    chmod 1777 tmp
    chmod 755 var/empty

    mkdir -p etc
    echo "127.0.0.1 localhost" > etc/hosts

    # Install static busybox for bootstrap (works when /nix is empty)
    cp ${busybox}/bin/busybox bin/busybox
    chmod 755 bin/busybox
    for cmd in sh cp mkdir chmod chown cat echo test ls sleep; do
      ln -sf busybox bin/$cmd
    done

    cp ${./entrypoint.sh} entrypoint.sh
    chmod +x entrypoint.sh

    # Fix Nix permissions for non-root users
    # Store must be writable to add new paths and create lock files
    chmod -R a+rwX nix/store nix/var/nix

    # Pre-create Nix directory structure
    mkdir -p nix/var/nix/profiles/per-user
    mkdir -p nix/var/nix/gcroots/per-user
    mkdir -p nix/var/log/nix/drvs
    chmod 755 nix/var/nix/profiles nix/var/nix/profiles/per-user
    chmod 755 nix/var/nix/gcroots nix/var/nix/gcroots/per-user
    chmod -R a+rwX nix/var/log

    # Note: Persistent store bootstrap is handled by the CLI (wrix-builder)
    # which copies /nix from a temp container before mounting the volume
  '';

  config = {
    Env = [
      "PATH=${builderEnv}/bin:/bin:/usr/bin"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      "NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      # Use /run to avoid VirtioFS permission issues
      "NIX_DAEMON_SOCKET_PATH=/run/nix/daemon.sock"
    ];
    Entrypoint = [ "/entrypoint.sh" ];
    ExposedPorts = {
      "22/tcp" = { };
    };
  };
}
