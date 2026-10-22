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
    if [[ "''${1-}" == invoked ]]; then
      printf '%s' "''${WRIX_INVOKED_BIN-}"
    elif [[ "''${1-}" == controller ]]; then
      printf '%s' "''${WRIX_CONTROLLER_PACKAGE_BINDING-}"
    else
      printf '%s' "''${WRIX_SERVICE_PACKAGE_BINDING-}"
    fi
  '';
  rustProfile.buildPackage = _: {
    bin = pkgs.runCommand "controller-package-fixture" { } ''
      set -euo pipefail

      mkdir -p "$out/bin"
      ln -s '${binary}/bin/wrix' "$out/bin/wrix"
      ln -s '${binary}/bin/wrix' "$out/bin/wrix-host-controller"
    '';
  };
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
  controllerMetadata = import ../../lib/services/control-binding.nix {
    pkgs = pkgs // {
      writeText = _: text: text;
    };
    controller = host.controllerPackage;
    runtime = binary;
    inherit image;
  };
  controllerContext = builtins.getContext controllerMetadata;
in
assert controllerContext.${unsafeDiscardStringContext source.drvPath} == { path = true; };
assert builtins.length (builtins.attrNames controllerContext) == 3;
assert builtins.length (builtins.attrNames context) == 2;
assert builtins.all (entry: entry == { path = true; }) (builtins.attrValues context);
pkgs.runCommand "host-service-package-binding"
  {
    nativeBuildInputs = [ pkgs.jq ];
  }
  ''
    set -euo pipefail
    test "$(${host.wrix}/bin/wrix)" = '${host.binding}'
    test "$(WRIX_INVOKED_BIN=stale ${host.wrix}/bin/wrix invoked)" = '${host.wrix}/bin/wrix'
    test "$(${guest.wrix}/bin/wrix invoked)" = '${guest.wrix}/bin/wrix'
    test "$(WRIX_SERVICE_PACKAGE_BINDING=project ${host.wrix}/bin/wrix)" = '${host.binding}'
    test -z "$(env -u WRIX_SERVICE_PACKAGE_BINDING ${guest.wrix}/bin/wrix)"
    test "$(WRIX_CONTROLLER_PACKAGE_BINDING=project ${host.wrix}/bin/wrix controller)" = '${host.controllerBinding}'
    test -z "$(env -u WRIX_CONTROLLER_PACKAGE_BINDING ${guest.wrix}/bin/wrix controller)"
    jq -e --arg source '${unsafeDiscardStringContext (toString source)}' \
      --arg drv '${unsafeDiscardOutputDependency source.drvPath}' \
      '.schema == 1 and .source.path == $source and .source.derivation == $drv' \
      '${host.binding}'
    jq -e --arg controller '${host.controllerPackage}' \
      --arg image '${unsafeDiscardOutputDependency source.drvPath}' \
      '.controller == $controller and .image == $image and (.runtime | startswith("/nix/store/"))' \
      '${host.controllerBinding}'
    test -e '${unsafeDiscardOutputDependency source.drvPath}'
    test -e '${unsafeDiscardOutputDependency digest.drvPath}'
    jq -e '.schema == 1 and .image != "" and (.source_kind == "nix-descriptor" or .source_kind == "docker-archive")' '${actualBinding}'
    for field in source digest; do
      drv="$(jq -r ".$field.derivation" '${actualBinding}')"
      test -f "$drv"
    done
    touch "$out"
  ''
