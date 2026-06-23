{
  pkgs,
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
  ociLayout =
    runCommandLocal "${imageName}-oci"
      {
        nativeBuildInputs = [
          jq
          skopeo
        ];
      }
      ''
        set -euo pipefail
        export HOME=$TMPDIR

        mkdir -p "$out"
        image_tar="$TMPDIR/image.tar"
        "${rawImage}" > "$image_tar"
        skopeo --insecure-policy copy --quiet \
          "docker-archive:$image_tar" "oci:$out:latest"
        mkdir -p "$out/wrix"
        manifest_json="$out/wrix/manifest.json"
        skopeo inspect --raw "oci:$out:latest" > "$manifest_json"
        config_digest=$(jq -er '.config.digest | select(test("^sha256:[0-9a-f]{64}$"))' "$manifest_json")
        printf '%s\n' "$config_digest" > "$out/wrix/config-digest"
        config_blob="$out/blobs/sha256/''${config_digest#sha256:}"
        jq -n \
          --slurpfile manifest "$manifest_json" \
          --slurpfile config "$config_blob" \
          '($manifest[0].layers // []) as $layers
           | ($config[0].rootfs.diff_ids // []) as $diffIds
           | {
               media_type: ($manifest[0].mediaType // ""),
               config: $manifest[0].config,
               layers: [
                 range(0; ($layers | length)) as $i
                 | $layers[$i] + { diff_id: ($diffIds[$i] // "") }
               ]
             }' \
          > "$out/wrix/descriptor-manifest.json"
      '';
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
      runCommandLocal "${imageName}-digest"
        {
          nativeBuildInputs = [
            jq
            skopeo
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
