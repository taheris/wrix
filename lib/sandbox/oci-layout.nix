{
  pkgs,
}:

{
  image,
  name,
}:

let
  inherit (pkgs) python3 runCommandLocal;
in
runCommandLocal name
  {
    nativeBuildInputs = [ python3 ];
  }
  ''
    set -euo pipefail
    python ${./make-oci-layout.py} ${image.conf} "$out"
  ''
