{ pkgs, wrix }:

let
  inherit (pkgs.lib) hasSuffix removeSuffix;
  integrationNames = map (removeSuffix ".rs") (
    builtins.filter (hasSuffix ".rs") (
      builtins.attrNames (builtins.readDir ../../crates/wrix-service/tests)
    )
  );
  normalClientGraph = pkgs.writeText "client-normal-graph.jq" ''
    INDEX(.packages[]; .name) as $packages |
    def reach($name): $name, ($packages[$name].dependencies[]? | select(.kind != "dev") | .name | reach(.));
    [reach("wrix-cli"), reach("wrix-cache"), reach("wrix-notify")] |
    all(.[]; . != "wrix-service-server" and (startswith("rama-") | not))
  '';
in
pkgs.runCommand "wrix-client-server-dependency-boundary"
  {
    nativeBuildInputs = [
      wrix.profiles.rust.toolchain
      pkgs.jq
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    mkdir -p "$HOME"
    cargo metadata --offline --locked --no-deps --format-version 1 \
      --manifest-path '${wrix.rustPackage.clientPackage.src}/Cargo.toml' > metadata.json
    jq -e -f '${normalClientGraph}' metadata.json
    jq '.packages |= map(if .name == "wrix-service" then .dependencies += [{kind:null,name:"rama-http"}] else . end)' metadata.json > forbidden.json
    if jq -e -f '${normalClientGraph}' forbidden.json; then
      echo 'normal client graph audit admitted a server dependency' >&2
      exit 1
    fi
    jq -e --argjson expected '${builtins.toJSON integrationNames}' '
      [.packages[] | select(.name == "wrix-service-server") | .targets[] | select(.kind == ["test"]) | .name] | sort == $expected
    ' metadata.json
    jq -e '
      .packages[] | select(.name == "wrix-service-server") |
      [.dependencies[] | select(.kind == null) | .name] |
      (index("rama-http") != null and index("rama-http-core") != null and index("rama-tcp") != null)
    ' metadata.json
    touch "$out"
  ''
