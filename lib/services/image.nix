{
  pkgs,
  cacheServe,
  asTarball ? false,
}:

let
  inherit (builtins) hashString toJSON unsafeDiscardStringContext;
  inherit (pkgs)
    coreutils
    dolt
    dockerTools
    jq
    runCommandLocal
    skopeo
    writeText
    ;

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
  buildImage = if asTarball then dockerTools.buildLayeredImage else dockerTools.streamLayeredImage;
  rawImage = buildImage {
    name = imageName;
    tag = "latest";

    inherit contents;

    extraCommands = ''
      mkdir -p tmp
      chmod 1777 tmp
    '';

    config = imageConfig;
  };
  sourceKind = if asTarball then "docker-archive" else "nix-descriptor";
  descriptorMetadata = {
    schema = 1;
    source_kind = "nix-descriptor";
    image = {
      name = imageName;
      tag = "latest";
    };
    materialized_roots = map toString contents;
    config = {
      env = imageConfig.Env;
      inherit labels;
    };
  };
  descriptorDigest = "sha256:${hashString "sha256" (unsafeDiscardStringContext (toJSON descriptorMetadata))}";
  nixDescriptorSource = writeText "${imageName}-nix-descriptor.json" (
    toJSON (
      descriptorMetadata
      // {
        digest = descriptorDigest;
        fallback_stream = "${rawImage}";
      }
    )
  );
  descriptorDigestFile = writeText "${imageName}-descriptor-digest" descriptorDigest;
  imageSource = if asTarball then rawImage else nixDescriptorSource;
  digestFile =
    if asTarball then
      runCommandLocal "${imageName}-digest"
        {
          nativeBuildInputs = [
            jq
            skopeo
          ];
        }
        ''
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
