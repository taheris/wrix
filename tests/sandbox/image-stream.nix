{ pkgs }:
let
  stream = import ../../lib/sandbox/image-stream.nix { inherit pkgs; };
  payload = pkgs.writeTextDir "share/image-stream-payload" "streaming image fixture\n";
  args = {
    name = "wrix-stream-fixture";
    tag = "test";
    contents = [ payload ];
    maxLayers = 2;
  };
  original = pkgs.dockerTools.buildLayeredImage args;
  raw = stream.dockerTools.streamLayeredImage args;
  compressed = stream.dockerTools.buildLayeredImage args;
  overridden = compressed.override { config.Env = [ "WRIX_STREAM_FIXTURE=1" ]; };
  unit = pkgs.runCommandLocal "docker-image-stream-unit" { } ''
    set -euo pipefail
    export PYTHONDONTWRITEBYTECODE=1
    ${pkgs.python3}/bin/python3 ${./test_image_stream.py} ${stream.upstreamSource} ${stream.patchedSource}
    touch "$out"
  '';
in
assert compressed.stream.streamScript == raw.streamScript;
assert overridden.stream.streamScript == raw.streamScript;
assert builtins.all (key: !(builtins.hasAttr key (builtins.getContext compressed.buildCommand))) (
  builtins.attrNames (builtins.getContext (toString original.stream))
);
pkgs.runCommandLocal "docker-image-stream" { passthru = { inherit unit; }; } ''
  set -euo pipefail
  test -e ${unit}
  ${pkgs.gnugrep}/bin/grep -F '${raw.streamScript}' ${raw} > /dev/null
  ${raw} > expected.tar
  ${pkgs.gzip}/bin/gzip -cd ${compressed} | ${pkgs.diffutils}/bin/cmp expected.tar -
  touch "$out"
''
