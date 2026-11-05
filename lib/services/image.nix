{
  pkgs,
  hostPkgs ? pkgs,
  celler ? null,
  supervisor ? null,
  pool ? null,
  asTarball ? false,
  hostResources ? null,
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
  compiler = import ../../crates/wrix-service/src/lifecycle/drain/native/compiler.nix {
    inherit pkgs;
  };

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
  stableContents = [
    dockerTools.binSh
    dockerTools.usrBinEnv
  ]
  ++ (
    if pool == null then
      [
        coreutils
        dolt
        celler
        compiler
        pkgs.rustc
        pkgs.stdenv.cc
        pkgs.curl
        util-linux
      ]
    else
      [
        pool.runtime
        pool.filesystem
        pool.bootstrap
      ]
  );
  contents = stableContents ++ pkgs.lib.optional (pool == null) supervisor;
  baseImage = imageBuilderPkgs.dockerTools.streamLayeredImage {
    name = "${imageName}-base";
    tag = "latest";
    architecture = pkgs.go.GOARCH;
    contents = stableContents;
    maxLayers = 90;
  };
  baseLayout = mkOciLayout {
    image = baseImage;
    name = "${imageName}-base-oci";
  };
  baseClosure = imageBuilderPkgs.closureInfo { rootPaths = stableContents; };
  leafLayering =
    imageBuilderPkgs.runCommandLocal "${imageName}-layering.json"
      { nativeBuildInputs = [ imageBuilderPkgs.jq ]; }
      ''
        set -euo pipefail
        jq -n --rawfile paths ${baseClosure}/store-paths '
          ($paths | split("\n") | map(select(length > 0))) as $base
          | [["remove_paths", $base], ["popularity_contest"], ["limit_layers", 30]]
        ' > "$out"
      '';
  imageConfig = {
    Env = [
      "HOME=${if pool == null then "/tmp" else "/var/empty"}"
      "PATH=${if pool == null then "/bin:/usr/bin" else "${pool.runtime}/bin:/bin:/usr/bin"}"
    ];
    Labels = labels;
  }
  // (if pool == null then { } else { Entrypoint = [ "${pool.bootstrap}/bin/wrix-nix-pool" ]; });
  rawImage = imageBuilderPkgs.dockerTools.streamLayeredImage (
    {
      name = imageName;
      tag = "latest";
      architecture = pkgs.go.GOARCH;

      contents = if pool == null then [ supervisor ] else contents;
      includeNixDB = pool != null;

      extraCommands = ''
        mkdir -p tmp
        chmod 1777 tmp
        ${if pool == null then "" else pool.imageCommands}
      '';

      config = imageConfig;
    }
    // pkgs.lib.optionalAttrs (pool == null) { layeringPipeline = leafLayering; }
  );
  sourceKind = if asTarball then "docker-archive" else "nix-descriptor";
  mkOciLayout = import ../sandbox/oci-layout.nix { pkgs = imageBuilderPkgs; };
  ociLayout = mkOciLayout {
    image = rawImage;
    name = "${imageName}-oci";
    fromLayout = if pool == null then baseLayout else null;
  };
  archive = import ../sandbox/docker-archive.nix {
    pkgs = imageBuilderPkgs;
    layout = ociLayout;
    name = imageName;
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
  imageSource = if asTarball then archive else nixDescriptorSource;
  digestFile =
    if asTarball then
      import ../sandbox/archive-digest.nix {
        pkgs = imageBuilderPkgs;
        layout = ociLayout;
        name = imageName;
      }
    else
      descriptorDigestFile;
  imageTag = imageTagLib.mkImageTag imageSource;
  refPrefix = if asTarball then "" else "localhost/";
in
assert pool != null || (celler != null && supervisor != null);
(if asTarball then archive else rawImage)
// {
  stream = rawImage;
  digest = digestFile;
  digestDerivation = if asTarball then digestFile else ociLayout;
  inherit labels ociLayout baseLayout;
  ref = "${refPrefix}${imageName}:${imageTag}";
  source = imageSource;
  source_kind = sourceKind;
  resources = (import ./resources.nix { inherit (pkgs) lib; }) hostResources;
}
