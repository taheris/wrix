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
    bash ${./oci-layout-reserved-output.sh} ${imageTools}/bin/wrix-image-tools ${storeConf} --store-layers-only
  '';
in
runCommandLocal name
  {
    passthru = { inherit storeLayers; };
  }
  ''
    set -euo pipefail
    bash ${./oci-layout-reserved-output.sh} ${imageTools}/bin/wrix-image-tools ${conf} \
      --store-layers ${storeLayers} \
      ${optionalString (fromLayout != null) "--from-layout ${fromLayout}"}
  ''
