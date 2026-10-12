{
  pkgs,
}:

{
  image,
  name,
  fromLayout ? null,
  storeImage ? image,
}:

let
  inherit (pkgs) python3 runCommandLocal;
  inherit (pkgs.lib) optionalString;
  inherit (((image.stream or image).override { fromImage = null; })) conf;
  storeConf = ((storeImage.stream or storeImage).override { fromImage = null; }).conf;
  storeLayers = runCommandLocal "${name}-store-layers" { nativeBuildInputs = [ python3 ]; } ''
    set -euo pipefail
    python ${./make-oci-layout.py} ${storeConf} "$out" --store-layers-only
  '';
in
runCommandLocal name
  {
    nativeBuildInputs = [ python3 ];
    passthru = { inherit storeLayers; };
  }
  ''
    set -euo pipefail
    python ${./make-oci-layout.py} ${conf} "$out" \
      --store-layers ${storeLayers} \
      ${optionalString (fromLayout != null) "--from-layout ${fromLayout}"}
  ''
