# Build the OCI image for wrix-builder (Linux remote builder)
#
# This creates a layered container image with:
# - nix-daemon for remote building
# - sshd for ssh-ng:// access
# - Builder user with UID 1000 for VirtioFS compatibility
#
{
  pkgs,
  hostPkgs ? pkgs,
  asTarball ? false,
}:

let
  inherit (builtins) toJSON;
  inherit (pkgs)
    buildEnv
    dockerTools
    jq
    runCommandLocal
    writeText
    writeTextDir
    ;

  imageBuilderPkgs = if asTarball then hostPkgs else pkgs;

  # Static busybox for bootstrapping when /nix volume is empty
  # Provides /bin/sh and basic commands independent of /nix/store
  inherit (pkgs.pkgsStatic) busybox;

  imageTagLib = import ../../util/image-tag.nix { };
  imageName = "wrix-builder";

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
  passwdFile = writeTextDir "etc/passwd" ''
    root:x:0:0:root:/root:/bin/bash
    builder:x:1000:1000:Nix Builder:/home/builder:/bin/bash
    sshd:x:74:74:Privilege-separated SSH:/var/empty:/bin/false
    nobody:x:65534:65534:Unprivileged account:/var/empty:/bin/false
  '';

  # group: root, users (with builder), sshd, nogroup
  groupFile = writeTextDir "etc/group" ''
    root:x:0:
    users:x:100:builder
    sshd:x:74:
    nogroup:x:65534:
  '';

  # sshd configuration: key-only auth, allow builder user
  # Note: entrypoint.sh regenerates this at runtime to handle persistent store mounts
  sshdConfig = writeTextDir "etc/ssh/sshd_config" ''
    Port 22
    ListenAddress 127.0.0.1
    HostKey /etc/ssh/ssh_host_ed25519_key
    AuthorizedKeysFile /home/%u/.ssh/authorized_keys
    PasswordAuthentication no
    PermitRootLogin no
    AllowUsers builder
    Subsystem sftp internal-sftp
  '';

  labels = {
    "wrix.managed" = "true";
    "wrix.image.kind" = "builder";
  };

  # Create merged environment with all packages
  builderEnv = buildEnv {
    name = "wrix-builder-env";
    paths = builderPackages;
    pathsToLink = [
      "/bin"
      "/share"
      "/etc"
      "/lib"
    ];
  };

  contents = [
    passwdFile
    groupFile
    sshdConfig
    dockerTools.usrBinEnv
    # Note: binSh omitted - we use static busybox instead so /bin/sh works
    # even when /nix is mounted empty for persistent store initialization
    dockerTools.caCertificates
    builderEnv
  ];

  imageConfig = {
    Env = [
      "PATH=${builderEnv}/bin:/bin:/usr/bin"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      "NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      # Use /run to avoid VirtioFS permission issues
      "NIX_DAEMON_SOCKET_PATH=/run/nix/daemon.sock"
    ];
    Entrypoint = [ "/entrypoint.sh" ];
    Labels = labels;
    ExposedPorts = {
      "22/tcp" = { };
    };
  };

  buildImage =
    if asTarball then
      imageBuilderPkgs.dockerTools.buildLayeredImage
    else
      dockerTools.streamLayeredImage;
  rawImage = buildImage {
    name = imageName;
    tag = "latest";
    architecture = pkgs.go.GOARCH;
    maxLayers = 50;
    includeNixDB = true;

    inherit contents;

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
      mkdir -p usr/lib/wrix-builder
      cp ${./sshd.sh} usr/lib/wrix-builder/sshd.sh
      chmod 644 usr/lib/wrix-builder/sshd.sh

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

    config = imageConfig;
  };

  sourceKind = if asTarball then "docker-archive" else "nix-descriptor";
  mkOciLayout = import ../oci-layout.nix { inherit pkgs; };
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
    oci_layout = "${ociLayout}";
    oci_ref = "latest";
    materialized_roots = map toString contents;
    config = {
      env = imageConfig.Env;
      entrypoint = imageConfig.Entrypoint;
      inherit labels;
    };
  };
  descriptorMetadataFile = writeText "${imageName}-descriptor-metadata.json" (
    toJSON descriptorMetadata
  );
  descriptorDigestFile = "${ociLayout}/wrix/config-digest";
  nixDescriptorSource =
    runCommandLocal "${imageName}-nix-descriptor.json"
      {
        nativeBuildInputs = [ jq ];
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
  digestFile =
    if asTarball then
      imageBuilderPkgs.runCommandLocal "${imageName}-digest"
        {
          nativeBuildInputs = [
            imageBuilderPkgs.jq
            imageBuilderPkgs.skopeo
          ];
        }
        ''
          set -euo pipefail
          export TMPDIR="$PWD/tmp"
          export HOME="$TMPDIR"
          mkdir -p "$TMPDIR"
          skopeo --tmpdir "$TMPDIR" --insecure-policy copy --quiet \
            "docker-archive:${rawImage}" "oci:$TMPDIR/image-oci:latest"
          skopeo --tmpdir "$TMPDIR" inspect --raw "oci:$TMPDIR/image-oci:latest" \
            | jq -r '.config.digest' > "$out"
        ''
    else
      descriptorDigestFile;
  imageTag = imageTagLib.mkImageTag rawImage;
  refPrefix = if asTarball then "" else "localhost/";
in
rawImage
// {
  digest = digestFile;
  inherit labels;
  ref = "${refPrefix}${imageName}:${imageTag}";
  source = imageSource;
  source_kind = sourceKind;
}
