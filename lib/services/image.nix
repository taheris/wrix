{
  pkgs,
  hostPkgs ? pkgs,
  celler ? null,
  supervisor ? null,
  pool ? null,
  asTarball ? false,
}:

let
  inherit (builtins) toJSON;
  inherit (pkgs)
    coreutils
    dolt
    dockerTools
    jq
    runCommandLocal
    util-linux
    writeText
    ;

  imageBuilderPkgs = if asTarball then hostPkgs else pkgs;

  imageTagLib = import ../util/image-tag.nix { };
  imageName = if pool == null then "wrix-service" else "wrix-nix-pool";
  labels = {
    "wrix.managed" = "true";
    "wrix.image.kind" = "service";
  }
  // (
    if pool == null then
      { "wrix.service.supervision" = "2"; }
    else
      {
        "wrix.nix-pool.bootstrap-version" = "3";
        "wrix.nix-pool.upgrade-version" = "1";
      }
  );
  contents = [
    dockerTools.binSh
    dockerTools.usrBinEnv
  ]
  ++ (
    if pool == null then
      [
        coreutils
        dolt
        celler
        supervisor
        util-linux
      ]
    else
      [
        pool.runtime
        pool.filesystem
        pool.bootstrap
      ]
  );
  imageConfig = {
    Env = [
      "HOME=${if pool == null then "/tmp" else "/var/empty"}"
      "PATH=${if pool == null then "/bin:/usr/bin" else "${pool.runtime}/bin:/bin:/usr/bin"}"
    ];
    Labels = labels;
  }
  // (if pool == null then { } else { Entrypoint = [ "${pool.bootstrap}/bin/wrix-nix-pool" ]; });
  buildImage =
    if asTarball then
      imageBuilderPkgs.dockerTools.buildLayeredImage
    else
      dockerTools.streamLayeredImage;
  rawImage = buildImage {
    name = imageName;
    tag = "latest";
    architecture = pkgs.go.GOARCH;

    inherit contents;
    includeNixDB = pool != null;

    extraCommands = ''
      mkdir -p tmp
      chmod 1777 tmp
      ${if pool == null then "" else pool.imageCommands}
    '';

    config = imageConfig;
  };
  sourceKind = if asTarball then "docker-archive" else "nix-descriptor";
  mkOciLayout = import ../sandbox/oci-layout.nix { inherit pkgs; };
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
assert pool != null || (celler != null && supervisor != null);
rawImage
// {
  digest = digestFile;
  digestDerivation = if asTarball then digestFile else ociLayout;
  inherit labels;
  ref = "${refPrefix}${imageName}:${imageTag}";
  source = imageSource;
  source_kind = sourceKind;
}
