{ pkgs, wrix }:

let
  inherit (builtins) unsafeDiscardOutputDependency unsafeDiscardStringContext;
  source = pkgs.runCommand "unrealized-service-source" { } "exit 99";
  digest = pkgs.runCommand "unrealized-service-digest" { } "exit 99";
  image = {
    ref = "localhost/wrix-service:binding-test";
    inherit source digest;
    source_kind = "nix-descriptor";
    digestDerivation = digest;
  };
  binary = pkgs.writeShellScriptBin "wrix" ''
    set -euo pipefail
    printf '%s' "''${WRIX_SERVICE_PACKAGE_BINDING-}"
  '';
  rustProfile.buildPackage = _: { bin = binary; };
  host = import ../../lib/services/rust.nix {
    inherit pkgs rustProfile;
    serviceImage = image;
  };
  guest = import ../../lib/services/rust.nix { inherit pkgs rustProfile; };
  actualBinding = wrix.rustPackage.binding;
  metadata = import ../../lib/services/binding.nix {
    pkgs = pkgs // {
      writeText = _: text: text;
    };
    image = wrix.serviceImage;
  };
  context = builtins.getContext metadata;
in
assert builtins.length (builtins.attrNames context) == 2;
assert builtins.all (entry: entry == { path = true; }) (builtins.attrValues context);
pkgs.runCommand "host-service-package-binding"
  {
    nativeBuildInputs = [ pkgs.jq ];
  }
  ''
    set -euo pipefail
    test "$(${host.wrix}/bin/wrix)" = '${host.binding}'
    test "$(WRIX_SERVICE_PACKAGE_BINDING=project ${host.wrix}/bin/wrix)" = '${host.binding}'
    test -z "$(env -u WRIX_SERVICE_PACKAGE_BINDING ${guest.wrix}/bin/wrix)"
    jq -e --arg source '${unsafeDiscardStringContext (toString source)}' \
      --arg drv '${unsafeDiscardOutputDependency source.drvPath}' \
      '.schema == 1 and .source.path == $source and .source.derivation == $drv' \
      '${host.binding}'
    test -e '${unsafeDiscardOutputDependency source.drvPath}'
    test -e '${unsafeDiscardOutputDependency digest.drvPath}'
    jq -e '.schema == 1 and .image != "" and (.source_kind == "nix-descriptor" or .source_kind == "docker-archive")' '${actualBinding}'
    for field in source digest; do
      drv="$(jq -r ".$field.derivation" '${actualBinding}')"
      test -f "$drv"
    done
    touch "$out"
  ''
