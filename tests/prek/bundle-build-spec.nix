{ pkgs }:

let
  hooks = import ../../lib/prek/bundle.nix { inherit pkgs; };
  inherit (builtins)
    concatStringsSep
    isBool
    isList
    mapAttrs
    removeAttrs
    toJSON
    ;
  envString =
    value:
    if value == null then
      ""
    else if isBool value then
      (if value then "1" else "")
    else if isList value then
      concatStringsSep " " (map toString value)
    else
      toString value;
in
pkgs.writeText "wrix-test-prek-build-spec.json" (toJSON {
  inherit (hooks.drvAttrs) builder args;
  env = mapAttrs (_: envString) (
    removeAttrs hooks.drvAttrs [
      "args"
      "builder"
      "__ignoreNulls"
    ]
  );
})
