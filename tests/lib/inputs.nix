# Resource operands supply both execution and its input projection. Opaque
# definitions do not use this constructor: their declarations remain unknown.
{
  root ? ../..,
}:

let
  inherit (builtins)
    all
    attrNames
    filter
    isList
    isString
    listToAttrs
    readFile
    stringLength
    substring
    toString
    ;
  nixTools = [
    "/flake.lock"
    "/loom.toml"
    "/patches/"
    "/${relative ./verifier.sh}"
    "/${relative ./print-inputs.sh}"
  ];
  rootPrefix = "${toString root}/";
  relative =
    path:
    let
      absolute = toString path;
      prefixLength = stringLength rootPrefix;
    in
    assert substring 0 prefixLength absolute == rootPrefix;
    substring prefixLength (stringLength absolute - prefixLength) absolute;
in
{
  project =
    definitions:
    listToAttrs (
      filter (entry: entry.value != null) (
        map (name: {
          inherit name;
          value =
            let
              inputs = definitions.${name}.inputs or null;
            in
            if inputs == null then
              null
            else
              assert isList inputs && all isString inputs;
              inputs;
        }) (attrNames definitions)
      )
    );
  read = path: {
    inputs = [ "/${path}" ];
    text = readFile (rootPrefix + path);
  };
  directory = path: {
    inherit path;
    relative = relative path;
    inputs = [ "/${relative path}/" ];
  };
  # Unknown import graphs retain the conservative repository-wide Nix scope.
  nix = inputs: [ "/**/*.nix" ] ++ nixTools ++ inputs;
  scopedNix = inputs: nixTools ++ inputs;
}
