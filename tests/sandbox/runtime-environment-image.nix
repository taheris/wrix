# Disposable protocol consumer, not a managed profile or service image.
{ pkgs }:
let
  runtimeEnvironment = import ../../lib/sandbox/runtime-environment.nix { inherit pkgs; };
  files = pkgs.runCommand "wrix-runtime-environment-consumer-files" { } ''
    mkdir -p "$out/run/wrix/runtime-environment"
    chmod 0700 "$out/run/wrix/runtime-environment"
    cat > "$out/entrypoint.sh" <<'SCRIPT'
    #!/bin/sh
    set -eu
    if (printf x >> /run/wrix/runtime-environment/environment) 2>/dev/null; then
      echo 'environment mount was writable' >&2
      exit 47
    fi
    printf '%s\000' "$OPENAI_API_KEY" "$EMPTY" "$MESSAGE" "$CONTAINER_HOST" "$WRIX_NETWORK_ENVIRONMENT_READY"
    SCRIPT
    chmod 0555 "$out/entrypoint.sh"
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "wrix-test-runtime-environment";
  tag = builtins.substring 0 12 (builtins.baseNameOf (toString runtimeEnvironment.package));
  contents = [
    pkgs.busybox
    runtimeEnvironment.package
    files
  ];
  maxLayers = 3;
  config = {
    Entrypoint = [ "/bin/wrix-runtime-env" ];
    Env = [ "PATH=/bin" ];
    Labels."wrix.runtime.environment" = toString runtimeEnvironment.version;
  };
}
