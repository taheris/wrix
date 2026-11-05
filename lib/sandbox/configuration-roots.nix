# Resolve output tokens against the immutable input closure at build time.
# Opaque source paths are already exact; do not expand .drv metadata into builds.
{ pkgs }:
inputs:
let
  inherit (pkgs) lib;
  candidates = import ./configuration-candidates.nix { inherit lib; } inputs;
  knownRoot =
    value:
    let
      context = builtins.getContext value;
      matches = builtins.filter (
        path:
        (context.${path}.path or false) && lib.hasPrefix path (builtins.unsafeDiscardStringContext value)
      ) (builtins.attrNames context);
      ordered = lib.sort (a: b: builtins.stringLength a < builtins.stringLength b) matches;
      path = lib.last ordered;
    in
    if matches == [ ] then null else builtins.appendContext path { ${path}.path = true; };
  known = lib.unique (builtins.filter (value: value != null) (map knownRoot candidates));
  dynamic = builtins.filter (value: knownRoot value == null) candidates;
  observations = pkgs.writeText "wrix-configuration-references.json" (builtins.toJSON dynamic);
  sources = pkgs.writeText "wrix-configuration-sources.json" (builtins.toJSON known);
  closure = pkgs.closureInfo { rootPaths = [ observations ]; };
in
if candidates == [ ] then
  null
else
  pkgs.runCommand "wrix-image-configuration" { } ''
    ${lib.getBin pkgs.python3}/bin/python3 ${./configuration-roots.py} ${observations} ${closure}/store-paths "$out" ${sources}
  ''
