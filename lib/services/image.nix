{
  pkgs,
  hostPkgs ? pkgs,
  cacheServe,
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
    writeText
    ;

  imageBuilderPkgs = if asTarball then hostPkgs else pkgs;

  imageTagLib = import ../util/image-tag.nix { };
  imageName = "wrix-service";
  labels = {
    "wrix.managed" = "true";
    "wrix.image.kind" = "service";
  };
  contents = [
    dockerTools.binSh
    dockerTools.usrBinEnv
    coreutils
    dolt
    cacheServe
  ];
  imageConfig = {
    Env = [
      "HOME=/tmp"
      "PATH=/bin:/usr/bin"
    ];
    Labels = labels;
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

    inherit contents;

    extraCommands = ''
      mkdir -p tmp
      chmod 1777 tmp
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
rawImage
// {
  digest = digestFile;
  inherit labels;
  ref = "${refPrefix}${imageName}:${imageTag}";
  source = imageSource;
  source_kind = sourceKind;
}
