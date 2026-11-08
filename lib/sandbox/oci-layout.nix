{
  pkgs,
}:

{
  image,
  name,
  fromLayout ? null,
  storeImage ? image,
  configuration ? null,
  storeConfiguration ? null,
}:

let
  inherit (pkgs) runCommandLocal;
  imageTools = import ./image-tools.nix { pkgs = pkgs.buildPackages; };
  inherit (pkgs.lib) optionalString;
  conf =
    if configuration == null then
      ((image.stream or image).override { fromImage = null; }).conf
    else
      configuration;
  storeConf =
    if storeConfiguration == null then
      ((storeImage.stream or storeImage).override { fromImage = null; }).conf
    else
      storeConfiguration;
  storeLayers = runCommandLocal "${name}-store-layers" { } ''
    set -euo pipefail
    ${imageTools}/bin/wrix-image-tools oci-layout ${storeConf} "$out" --store-layers-only
  '';
in
runCommandLocal name
  {
    passthru = { inherit storeLayers; };
  }
  ''
    set -euo pipefail
    ${imageTools}/bin/wrix-image-tools oci-layout ${conf} "$out" \
      --store-layers ${storeLayers} \
      ${optionalString (fromLayout != null) "--from-layout ${fromLayout}"}
  ''
